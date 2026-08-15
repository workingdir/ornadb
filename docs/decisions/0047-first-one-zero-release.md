# ADR 0047: The First 1.0 Release Uses One Authenticated Debian Authority

**Status:** Accepted

## Context

Orna can build and test the current `0.1.0-1` development Debian package, but
a retained CI package is not a production release. Development packaging and
protected release packaging need different entry points and gates. The
repository also does not yet define the complete product scope that must be
accepted for Orna 1.0.

This decision fixes the release mechanism for a future first production
release. It does not state that the current language, server, operational,
documentation, or compatibility scope is complete.

## Decision

The first production release is the Debian 12 amd64 package version
`1.0.0-1`. Its upstream product version is `1.0.0`, its Debian revision is
`1`, and its source tag is `v1.0.0`.

The Debian build has two explicit modes and no implicit mode:

* `make -f packaging/debian/rules development-package` accepts only the
  current `orna (0.1.0-1) UNRELEASED; urgency=medium` changelog entry. It does
  not require a source tag, cannot request a production signature, and cannot
  publish. Ordinary CI uses only this mode.
* `make -f packaging/debian/rules release-package` is the protected mode. For
  the first release, it accepts only the released
  `orna (1.0.0-1) bookworm; urgency=medium` entry and exact signed `v1.0.0`
  tag. It requires every product and release gate in this decision.

A dry-run mode may validate an accepted mode and report the derived identity.
It must not write a package, a repository generation, a signature, a manifest,
or another persistent release artefact. A dry run cannot replace a build,
reproduction, signing, approval, or publication gate.

The generic `package` target must not select between these modes. A caller
must name one mode. Neither an environment variable nor a command-line
version override can change the accepted version or release state.

The authenticated Debian package and repository remain the only production
distribution authority under work ADR 0019. A CI retention artifact, a local
`.deb`, a GitHub release attachment, an executable, a manifest, a software
bill of materials (SBOM), or a detached signature is not a second production
distribution authority.

Acceptance of this release mechanism does not authorise the `1.0.0` version
change or publication. A separate accepted 1.0 product acceptance baseline is
mandatory before the release authority can declare, tag, sign, or publish
`1.0.0-1`.

## Canonical release identity

`packaging/debian/changelog` is the sole checked-in authority for the complete
Debian package version and release state. During development its first entry
must declare exactly:

```text
orna (0.1.0-1) UNRELEASED; urgency=medium
```

The final release commit replaces that entry with exactly:

```text
orna (1.0.0-1) bookworm; urgency=medium
```

The protected entry must be complete, dated, and released. Protected mode
cannot use `UNRELEASED`; development mode cannot use `bookworm` or another
released suite. `dpkg-parsechangelog` supplies both builders with the complete
Debian version, upstream version, Debian revision, source package name, target
suite, and release state. The build and publication workflows must not contain
another manually maintained package-version literal.

The `version` field in `Cargo.toml`'s `[workspace.package]` table owns one
upstream product version. Every crate manifest uses `version.workspace = true`;
no child manifest contains a product-version literal. The public
`orna --version` output uses
`env!("CARGO_PKG_VERSION")` from `orna-server` and prints exactly
`orna <upstream-version>` followed by one newline.

The following protected-mode identities must agree before a release build
starts:

| Identity | Required value or derivation |
| --- | --- |
| Debian source and binary package version | exact `1.0.0-1` from `packaging/debian/changelog` |
| Cargo workspace version | exact upstream part `1.0.0` |
| Public command version | exact upstream part `1.0.0` |
| Distribution manifest product version | exact upstream part `1.0.0` |
| Distribution manifest Debian version | exact complete version `1.0.0-1` |
| Package filename and repository record | derived from the complete Debian version, package name, and architecture |
| Source tag | exact `v1.0.0` on the release commit |

Development mode must reject any identity other than the exact unreleased
`0.1.0-1` development identity. Protected mode must reject a missing
changelog, an unreleased entry, another suite, another Debian revision, a
Cargo or command mismatch, a dirty source tree, a tag mismatch, or a version
supplied through an environment variable. The release commit is the exact
source authority. The tag identifies that commit but cannot change its
contents.

## Package copyright, licences, notices, and SBOM

The production package must install this exact release-document inventory:

```text
/usr/share/doc/orna/changelog.Debian.gz
/usr/share/doc/orna/copyright
/usr/share/doc/orna/POSTGRESQL-LICENSE
/usr/share/doc/orna/THIRD-PARTY-NOTICES
/usr/share/doc/orna/sbom.spdx.json
```

The files have these owners:

* `packaging/debian/changelog` owns the source text for
  `changelog.Debian.gz`. The package build compresses it reproducibly.
* `packaging/debian/copyright` is the checked-in Debian copyright and licence
  document authority. It owns the Debian rendering and the Orna-owned and
  embedded PostgreSQL mappings. The Orna-owned source remains `Apache-2.0`.
  Its external Rust sections are an exact projection of
  `dependency-licences.toml`; they cannot introduce an independent component,
  holder, no-holder proof, or licence authority. A package-level declaration
  cannot replace a required licence text.
* The embedded PostgreSQL build accepted by work ADR 0019 owns the exact
  `POSTGRESQL-LICENSE` bytes from the pinned PostgreSQL source. The Debian
  package copies those bytes without rewriting them.
* `packaging/debian/dependency-licences.toml` is the checked-in exact external
  Rust dependency licence and source inventory. Its primary key is the
  Cargo.lock triple `(name, version, checksum)`. Records are sorted by the
  byte order of name, version, then checksum. A name and version without the
  Cargo.lock checksum is not an identity. Orna workspace packages are owned by
  the Orna copyright record instead of this external inventory. Before any
  inventory record is added, each external record must contain these exact
  common TOML fields:

  ```toml
  name = "<Cargo package name>"
  version = "<Cargo package version>"
  checksum = "<Cargo.lock checksum>"
  cargo_lock_source = "<exact Cargo.lock source>"
  canonical_source_url = "<canonical source URL>"
  cargo_licence_raw = "<exact Cargo.toml license value>"
  normalized_spdx_licence = "<normalised SPDX expression>"
  selected_licence = "<one complete top-level SPDX OR branch>"
  selected_licence_inputs = [{ path = "<source-relative path>", sha256 = "<lowercase SHA-256>" }]
  notice_inputs = [{ path = "<source-relative path>", sha256 = "<lowercase SHA-256>" }]
  copyright_holders = ["<named upstream holder>"]
  # Or, only for the closed generic-MIT branch:
  # generic_mit_no_holder = true
  ```

  A revision-based `cargo_lock_source` must also contain
  `source_revision = "<exact source revision>"`. All other records must not
  contain `source_revision`. No other record field is permitted. The Debian
  copyright source rendering is derived only from `canonical_source_url`. It
  must not copy or maintain a second source URL. `cargo_licence_raw` is the
  byte-exact Cargo `license` field.
  `normalized_spdx_licence` is its SPDX normalisation. A slash (`/`) in
  `cargo_licence_raw` is an alternative-licence separator and normalises
  exactly to ` OR `. For example, `MIT/Apache-2.0` normalises to
  `MIT OR Apache-2.0` before SPDX parsing and selection. The generator parses
  `normalized_spdx_licence` into complete top-level `OR` branches.
  `selected_licence` must equal one complete branch, with every `AND` and
  `WITH` term preserved. It must not select one identifier from an `AND`
  branch. For example, `MIT AND Apache-2.0 OR BSD-3-Clause` can select either
  `MIT AND Apache-2.0` or `BSD-3-Clause`, but not `MIT`.

  Every registry record omits `source_revision`. The release-evidence
  generator must first verify the Cargo.lock checksum against the exact
  registry crate archive, then parse that checksum-bound archive's
  `.cargo_vcs_info.json`. It must require `git.sha1` to be exactly 40
  lowercase hexadecimal characters and must use the exact `path_in_vcs`
  string in that file. It must reject a missing, malformed, ambiguous, or
  checksum-mismatched archive or VCS file. It must read the `repository`
  value from that same archive's Cargo manifest and use those exact bytes as
  `canonical_source_url`, without normalisation. It must reject a missing,
  malformed, or mismatched manifest repository value. The SBOM must record
  the parsed VCS revision and `path_in_vcs` for every registry package. For
  each registry package, SPDX 2.3 `packages[].downloadLocation` must be
  exactly `git+<canonical_source_url>@<git.sha1>` when `path_in_vcs` is empty,
  or exactly `git+<canonical_source_url>@<git.sha1>#<path_in_vcs>` when it is
  non-empty. The empty-path form must omit `#` entirely. The generator must
  preserve the `canonical_source_url` bytes without URL normalisation and
  reject a nonconforming download-location encoding.

  Every input object has exactly `path` and `sha256`. Paths are relative to the
  locked source root. SHA-256 values are lowercase hexadecimal digests of the
  exact input bytes. `selected_licence_inputs` contains every selected licence
  input. `notice_inputs` contains every required notice input and is `[]` only
  when the selected obligation requires no notice. Each record uses exactly one
  attribution branch: a non-empty `copyright_holders` array, or
  `generic_mit_no_holder = true`. The generic branch omits
  `copyright_holders`; an empty holder array, a false generic flag, and both
  branches together are invalid.
* The release-evidence generator derives the Rust release closure from Cargo's
  unit graph. It runs this exact command inside the accepted pinned Debian
  builder, with the same release-build environment and flags as the final
  `orna-server` build:

  ```sh
  RUSTC_BOOTSTRAP=1 cargo -Z unstable-options build --unit-graph --frozen --offline --release --manifest-path /workspace/Cargo.toml --package orna-server --bin orna > cargo-unit-graph.json
  ```

  The generator accepts only unit-graph schema version `1`, exactly one root
  unit, and dependency indices that identify units in the graph. It traverses
  only units reachable from that root. Each reached registry unit must map
  uniquely to one Cargo.lock triple `(name, version, checksum)`. It rejects a
  missing, ambiguous, changed, differently sourced, or checksum-mismatched
  mapping. The reachable graph contains exactly 81 unique external registry
  packages.

  The runtime traversal starts at the same root. It does not traverse an edge
  whose child unit has target kind `proc-macro` or `custom-build`, or mode
  `run-custom-build`. Its unique external registry package set contains
  exactly 73 packages. This is the final executable dependency closure.

  The SPDX 2.3 relationship triples use the unit-graph edge direction. For a
  runtime edge `U -> V`, the generator emits
  `Package(U) DEPENDS_ON Package(V)`. For a proc-macro child edge `U -> V`, it
  emits `Package(V) BUILD_TOOL_OF Package(U)`. For an ordinary child edge
  `U -> V` where `U` has target kind `custom-build`, it emits
  `Package(V) BUILD_DEPENDENCY_OF Package(U)`. An edge from a package unit to
  its own `custom-build` or `run-custom-build` unit emits no package-level
  triple. A dependency edge from a proc-macro package to its ordinary
  dependency also emits `Package(U) DEPENDS_ON Package(V)`. The generator must
  deduplicate identical triples and reject a projected self-triple, a
  contradictory triple, an unsupported relationship, or a relationship that
  does not correspond to one reached unit-graph edge.

  `--unit-graph` is an unstable Cargo interface. The accepted builder must pin
  the Cargo version. The generator must reject a different Cargo version, a
  different unit-graph schema version, or an unrecognised graph field or
  variant before it uses closure data.

  `dependency-licences.toml` is a strict document container. It has exactly
  `format = 1`, one `[closure]` table, and sorted `[[dependency]]` tables. The
  `[closure]` table has exactly these required keys:

  ```toml
  format = 1

  [closure]
  cargo_version = "<accepted pinned Cargo version>"
  unit_graph_schema_version = 1
  root_package = "orna-server"
  root_binary = "orna"
  target = "x86_64-unknown-linux-gnu"
  external_registry_package_count = 81
  runtime_external_registry_package_count = 73
  ```

  `[[dependency]]` tables are sorted by the byte order of `name`, `version`,
  then `checksum`. The generator must reject an unknown top-level key, closure
  key, or dependency key; a missing container element; a different format; or
  a count that does not equal the selected unit-graph closure.
* The deterministic release-evidence generator owns
  `THIRD-PARTY-NOTICES`. It reads only the locked Rust closure, the pinned
  `dependency-licences.toml` records, the pinned PostgreSQL source and
  prepared-source inventories, and the changelog identity. It emits every
  notice required by code or data present in the final package. An SBOM
  identifier is not a substitute for required notice text.
* The same generator owns `sbom.spdx.json`. It emits deterministic SPDX 2.3
  JSON for `/usr/bin/orna`, the 73-package final executable Rust closure, the
  build and proc-macro relationships defined above, the embedded PostgreSQL
  source and support assets, and the other package data inputs. It records
  package versions, source revisions, checksums, declared and concluded
  licences, and relationships. It does not hash the enclosing
  `.deb`, itself, or the later distribution manifest. The distribution
  manifest and signed repository chain bind those final bytes without a
  circular checksum.

Generic MIT attribution is a closed exception. It is permitted only when
`selected_licence` is the complete branch exactly `MIT`,
`normalized_spdx_licence` offers that branch, `notice_inputs = []`, and
`selected_licence_inputs` contains exactly one MIT input. Its exact
`{path, sha256}` identity must be one of these four reviewed values:

```text
{ path = "LICENSE-MIT", sha256 = "23f18e03dc49df91622fe2a76176497404e46ced8a715d9d2b67a7446571cca3" }
{ path = "LICENSE-MIT", sha256 = "36516aefdc84c5d5a1e7485425913a22dbda69eb1930c5e84d6ae4972b5194b9" }
{ path = "LICENSE-MIT.md", sha256 = "fd80a26fbb3f644af1fa994134446702932968519797227e07a1368dea80f0bc" }
{ path = "LICENSE-MIT", sha256 = "508a77d2e7b51d98adeed32648ad124b7b30241a8e70b2e72c99f92d8e5874d1" }
```

This is the complete ADR allowlist. A new selected MIT input identity,
including a new path with an allowlisted digest or one whose text appears to
name no holder, requires an amendment to this decision before it can use
`generic_mit_no_holder = true`. The generator must not infer an absent holder
from new, changed, or unlisted licence text.

For an accepted generic-MIT record, the selected licence, SBOM concluded
licence, and Debian copyright rendering must each be exactly `MIT`. The
generic-MIT Debian copyright rendering must use the exact
`canonical_source_url` value. Protected mode must reject the generic-MIT path
if any generic condition, input path, digest, source URL derivation, or
allowlist membership fails. It must also reject a record that selects a
non-MIT licence, retains a combined selected obligation, has any required
notice, or omits a required named holder. Generic MIT cannot replace a
non-MIT obligation, a combined licence obligation, or another required notice
with generic MIT text.

Generic MIT attribution does not relax Cargo.lock identity, source, revision,
digest, closure, notice, or bidirectional inventory checks. The generator
must validate every selected licence and notice input against its pinned path
and digest. It must render all required licence and notice text exactly as
required by the pinned inputs.

`project_licence_text_input` is a separate closed exception. It is the
only permitted additional dependency-record field, and protected mode accepts
it only for this exact Cargo.lock identity:

```toml
name = "siphasher"
version = "1.0.3"
checksum = "8ee5873ec9cce0195efcb7a4e9507a04cd49aec9c83d0389df45b1ef7ba2e649"
cargo_lock_source = "registry+https://github.com/rust-lang/crates.io-index"
canonical_source_url = "https://github.com/jedisct1/rust-siphash"
cargo_licence_raw = "MIT/Apache-2.0"
normalized_spdx_licence = "MIT OR Apache-2.0"
selected_licence = "Apache-2.0"
selected_licence_inputs = [{ path = "COPYING", sha256 = "c962ee4d1d05ddc138b202b2540219ebc57893fcf97b364852094a9a94ce1365" }]
project_licence_text_input = { path = "LICENSE", sha256 = "cfc7749b96f63bd31c3c42b5c471bf756814053e847c10f3eb003417bc523d30" }
notice_inputs = []
copyright_holders = ["2012-2016 The Rust Project Developers", "2016-2026 Frank Denis"]
```

For this one record, `COPYING` is the upstream declaration and attribution
input. It remains bound to the locked siphasher crate archive and its VCS
evidence. `project_licence_text_input.path` is relative to the accepted Orna
project source root at the exact release commit, not to the locked crate
source root or to the siphasher VCS root. It must identify the regular,
non-symlink project-root `LICENSE` file. That file supplies the required
complete Apache-2.0 text. This exception does not change the source-relative
semantics of `selected_licence_inputs` or `notice_inputs`. Protected mode must
reject any deviation from this identity, source, licence selection, path, file
type, digest, empty-notice state, or holder list. It must reject the input if
the project source root or release commit differs from the accepted release
source authority. It must also reject `project_licence_text_input` on every
other record. This exception does not change the generic-MIT rules.

Acceptance invariant: for every registry package, the checked Cargo.lock
checksum, archive VCS revision, archive `path_in_vcs`, exact manifest
repository URL, SBOM VCS fields, and exact `packages[].downloadLocation`
encoding agree. For `siphasher 1.0.3`, the record must additionally equal the
complete closed exception above, including the regular Orna project-root
`LICENSE` input at the accepted release commit; otherwise the protected
release fails.

The package build must fail on an unknown component, a missing copyright or
licence mapping, a required but missing notice, a component present in only
one of the notice and SBOM views, a non-deterministic output, or a difference
between an SBOM checksum and the final package input. It must also fail on a
missing or false generic-MIT no-holder proof. It must generate the notice and
SBOM after the final dependency closure is fixed and before the distribution
manifest is finalised.

The generator must compare the complete selected external Cargo.lock closure
with `dependency-licences.toml` in both directions. It rejects a missing,
extra, duplicate, unsorted, differently sourced, or checksum-mismatched
record. An external dependency without a Cargo.lock checksum is outside the
first release policy and fails protected mode until a later accepted
source-identity rule replaces this constraint.

The distribution manifest binds the exact SHA-256 digest of each installed
release document and of the final package payload inventory. The signed Debian
repository metadata then binds the exact `.deb` bytes. The copyright file,
notices, SBOM, and manifests provide evidence. None is a signing or publication
authority.

## Checked-in release policy

`packaging/debian/release-policy.toml` is the sole checked-in release trust and
publication policy. Its strict, versioned schema contains:

* the full allowed OpenPGP fingerprints for signed source-tag authorities,
  with activation time, retirement time when set, and `active` or `revoked`
  state;
* the one full active Debian repository signing-key fingerprint;
* the exact path and SHA-256 digest of the checked-in public keyring
  `packaging/debian/orna-archive-keyring.gpg`;
* the accepted package name, `bookworm` suite and codename, `main` component,
  `amd64` architecture, tag pattern, and 14-day repository validity period;
  and
* the publisher format, repository generation format, and required protected
  approval class.

The policy contains public trust facts only. It contains no private key,
passphrase, credential, signing socket, repository endpoint, storage token, or
approval secret. The protected environment supplies those capabilities after
policy validation.

Protected mode verifies a source tag with OpenPGP and accepts it only when the
full primary-key fingerprint or full signing-subkey fingerprint maps to one
active, time-valid policy authority. Short key identifiers are forbidden. The
Debian signer must return a signature whose full fingerprint equals the one
active repository fingerprint and whose public key bytes equal the checked-in
keyring.

Key rotation is an append-first policy change. A signed policy commit adds the
new public key and fingerprint before first use, then a later signed policy
commit retires the old authority after the overlap period. Revocation is a
signed policy change that marks the fingerprint `revoked`; protected mode
rejects it immediately for new tags or repository metadata. Historical
fingerprints remain recorded and cannot become active again without a new
accepted release-security decision. Replacing keyring bytes, changing a
fingerprint, or changing a validity window is a reviewed policy change and
cannot occur inside a release job.

## Debian signing and publication authority

The release authority owns the Debian repository signing key and the final
publication decision. The private key stays in the protected release signer.
It cannot enter source control, a developer host, an ordinary pull-request or
push job, a CI retention artifact, the `.deb`, or the installed system.

The protected publisher must receive the exact reproduced `.deb` and release
evidence from the accepted release commit. Before signing, it must:

1. verify the signed `v1.0.0` tag and its exact commit;
2. verify the accepted 1.0 product baseline for that commit;
3. rebuild the package twice in the accepted release environment and compare
   the exact `.deb`, manifests, notices, SBOM, and installed inventory;
4. run the complete work ADR 0019 clean-machine package and lifecycle gate;
5. verify the exact version, suite, architecture, predecessor, licence, and
   SBOM rules in this decision; and
6. require a separate explicit publication approval from the release
   authority.

`packaging/debian/publish-repository.sh` is the one publisher interface. It
accepts only this command shape:

```text
packaging/debian/publish-repository.sh publish \
  --policy packaging/debian/release-policy.toml \
  --candidate <verified-candidate-directory> \
  --generation <unsigned-decimal-generation> \
  --expected-current <unsigned-decimal-generation> \
  --approval <protected-approval-file>
```

The protected environment supplies the signer and repository capabilities.
The interface does not accept an endpoint, key path, key fingerprint,
publication time, validity duration, package version, suite, component,
architecture, or output path from the caller. It reads those public facts from
the checked-in policy, reads the package identity from the verified candidate,
and reads publication time from the protected publisher's trusted UTC clock
after approval.

The publisher creates one new immutable
`generations/<20-digit-zero-padded-generation>/` tree. It writes the package
to `pool/main/o/orna/orna_1.0.0-1_amd64.deb` within that tree. It writes the
exact index to `dists/bookworm/main/binary-amd64/Packages` and its reproducible
compressed form to `Packages.gz`. Both records contain the exact package size
and SHA-256 digest. It then generates `dists/bookworm/Release` with the exact
index digests, suite, codename, `main` component, `amd64` architecture,
generation, canonical `Date`, and a `Valid-Until` exactly 14 days after
`Date`. The signed custom field is exactly `X-Orna-Generation`. The protected
Debian key signs those exact `Release` bytes as both `InRelease` and
`Release.gpg`.

The generation must equal `expected-current + 1`. The publisher compares
`expected-current` with the durable promoted generation and rejects a stale,
equal, skipped, reused, non-decimal, or overflowing generation before signing.
Before the first publication, the durable current generation is exactly zero
and no generation-zero tree exists.
It also rejects an existing generation path or any byte difference at an
existing immutable package path. After it has written, synchronised, and
verified the complete signed generation, it atomically promotes the repository
`current` reference with a compare-and-swap against `expected-current`.
Clients can see either the prior complete generation or the new complete
generation. A failed promotion leaves the prior `current` unchanged and the
new unpromoted generation cannot become a client authority.

`Date` is the protected UTC clock value at whole-second precision. It must be
later than the promoted generation's signed `Date`; clock rollback fails
before signing. `Valid-Until` is derived only by adding exactly 14 times 24
hours to that value. Neither field can come from candidate content or caller
input.

The supported installation path uses APT with the exact checked-in Orna public
key installed in its dedicated keyring. Its source configuration names that
keyring with `signed-by` and explicitly enables `Check-Valid-Until`. APT must
verify the repository signature, the signed index chain, the `.deb` digest,
and the 14-day expiry before dpkg receives the package. The supported client
configuration cannot set `check-valid-until=no`, `trusted=yes`,
`allow-insecure=yes`, or another expiry or signature bypass. An unsigned
repository, an expired or wrong key, changed metadata, a changed package, a
missing hash, or a partial publication fails closed.

The keyring reaches a client through a separately authenticated bootstrap
channel. A client cannot download the keyring from the unauthenticated form of
the repository that the keyring is intended to authenticate.

The `.deb` has no independent embedded or detached product signature. Its
authentication comes from the signed Debian repository chain required by work
ADR 0019.

This is Debian repository authentication, not a detached runtime signature.
Work ADR 0017's superseded Ed25519 runtime archive, accepted-runtime record,
runtime key, runtime signature, and manifest-first verification scheme must
not return. The embedded-engine and distribution manifests remain integrity
bindings inside the authenticated package as required by work ADR 0019.

## First-release predecessor rule

`1.0.0-1` is the first production release. It accepts no production package
or embedded-engine predecessor. Its distribution manifest contains exactly:

```text
accepted_predecessor_engines = []
supported_forward_edges = []
```

The production gate uses a clean Debian 12 amd64 installation. An existing
development or `0.x` installation is not an accepted upgrade source and does
not gain a compatibility promise from the 1.0 release. Package maintenance
must not convert, relabel, or open its durable instance as a 1.0 instance.
The protected `1.0.0-1` maintainer scripts accept only Debian's first-install
call shapes. They reject every `upgrade <old-version>` call before the package
maintenance `begin` operation or any durable state change. Development-mode
package update tests remain non-production evidence and cannot create a
production predecessor edge.

For a clean 1.0 instance, `orna server upgrade` proves the current-engine
no-op and exits successfully. It rejects every other recorded engine before
entering PostgreSQL. This preserves work ADR 0019's first-release rule. The
first later release that accepts a predecessor must name the exact predecessor
engine and forward edge and must implement the durable transition before it
can publish.

## CI retention artifacts

The existing seven-day workflow upload is short-lived review and diagnosis
evidence. It is not a release candidate repository, a mirror, an operator
installation path, or a production distribution. It can never satisfy the
publication gate, even when its package bytes later match the released bytes.

Ordinary CI has read-only repository permissions, no Debian signing key, no
publication credential, and no production repository write path. Its artifact
name and workflow output must identify the upload as non-production evidence.
Only the protected publisher can move independently reproduced bytes into the
authenticated Debian repository.

Ordinary CI invokes only `development-package`. It must fail if that target
selects a released changelog entry, accepts `1.0.0-1`, requests the protected
signer, or calls the publisher. Protected release infrastructure invokes only
`release-package` and must fail if it sees `UNRELEASED` or `0.1.0-1`.

## Release-mechanism acceptance criteria

The release mechanism is implemented only when tests prove all of these facts:

* development mode accepts only the exact unreleased `0.1.0-1` identity and
  cannot sign or publish, while protected mode accepts only the exact released
  `1.0.0-1` identity and complete release gates;
* the root workspace version is the only Cargo product-version literal, every
  child manifest inherits it, `orna --version` has the exact output above, and
  every checked-in mismatch fails before compilation while every generated
  mismatch fails before signing;
* two isolated release builds produce byte-identical `.deb`, manifest,
  changelog, copyright, licence, notice, SBOM, and payload bytes;
* the exact release-document inventory is installed, manifest-bound, and
  complete for the final embedded dependency closure;
* every selected external Cargo.lock `(name, version, checksum)` has exactly
  one matching checked-in dependency licence and source record, and every
  extra, missing, changed, unsorted, or checksum-less external record fails;
* the pinned Cargo unit graph has schema version `1`, one root, valid
  dependency indices, and exactly 81 unique reachable external registry
  packages; its runtime traversal has exactly 73 external registry packages;
  every package maps uniquely to Cargo.lock; and the emitted SPDX 2.3
  relationship triples have the required direction, type, deduplication, and
  contradiction rejection;
* every dependency-licences record has exactly the declared common and
  conditional TOML fields, applies slash licence normalisation to `OR`,
  selects one complete top-level SPDX `OR` branch without dropping `AND` or
  `WITH` terms, derives the Debian source rendering only from
  `canonical_source_url`, pins every selected licence and notice input by path
  and digest, and uses exactly one holder branch;
* every registry record has a checksum-bound crate archive, valid VCS revision
  and `path_in_vcs`, exact unnormalised Cargo manifest repository URL, and
  matching SBOM VCS fields and exact `packages[].downloadLocation`
  `git+<canonical_source_url>@<git.sha1>` encoding, with
  `#<path_in_vcs>` only for a non-empty path. The closed `siphasher 1.0.3`
  exception alone may use
  `project_licence_text_input`, and it must bind the exact regular Orna
  project-root `LICENSE` input to the accepted release commit; every other
  record or any mismatch fails;
* generic MIT attribution is accepted only when `selected_licence` is the
  complete `MIT` branch, the normalised SPDX expression offers that branch,
  there is exactly one selected MIT input whose `{path, sha256}` pair is in
  this decision's four-value allowlist, and `notice_inputs = []`; its
  selected, concluded, and Debian-rendered licence must each be exactly `MIT`.
  A new or unlisted input identity, a required notice, a non-MIT or combined
  selected obligation, an absent, changed, omitted, or mismatched source URL
  or input, an omitted named holder, another rendered or concluded licence, or
  automatic no-holder inference fails;
* the SPDX 2.3 SBOM and third-party notices agree with each other and with the
  final package, and changed or unknown closure members fail the build;
* protected mode accepts only an active policy-listed full OpenPGP source-tag
  signer fingerprint and exact policy-bound Debian signer and public keyring,
  while unknown, retired, revoked, short, changed, or out-of-window identities
  fail before publication;
* the protected publisher accepts only the exact signed tag commit, accepted
  product baseline, successful production gates, and explicit publication
  approval;
* a clean APT client accepts the signed repository chain and package, while
  wrong-key, unsigned, expired, changed, and partial states fail closed;
* publication creates one immutable next generation, rejects stale, equal,
  skipped, reused, or raced generations, preserves the prior current state on
  failure, atomically promotes only a complete signed generation, and rejects
  replay of a prior generation;
* `Date` is canonical UTC, `Valid-Until` is exactly 14 days later, and the
  supported APT configuration enforces signature and expiry checks without a
  bypass;
* first installation succeeds, every package predecessor is rejected before
  package maintenance begins, and current-engine upgrade no-op and
  foreign-engine rejection preserve the empty predecessor and edge sets;
* ordinary CI can retain non-production evidence but cannot sign or publish;
* no runtime archive, runtime Ed25519 key, detached runtime signature, or
  accepted-runtime record is generated, installed, or published; and
* no RPM package or RPM repository is built, signed, tested, or published.

Passing these criteria accepts only the mechanism. It does not accept the
product scope and does not permit a 1.0 release by itself.

## Later 1.0 product acceptance baseline

Before any commit changes the declared product version to `1.0.0`, a separate
versioned 1.0 product acceptance baseline must be accepted in the repository.
That baseline must name the complete supported public language, commands,
protocols, persistence, security, installation, recovery, upgrade,
compatibility, operator documentation, and known-limit surface for the first
production release. It must map each mandatory claim to exact required
evidence and must state each deferred surface without implying support. That
evidence must pass on the baseline commit and again on the final release
commit.

The baseline is a product-scope decision, not a checklist inferred from this
ADR, the current TODO, passing unit tests, or the existence of a package. The
release authority must verify its explicit acceptance and evidence before it
permits the `1.0.0` version commit, signed tag, repository signature, or
publication. A missing, proposed, incomplete, waived, or failing baseline
stops the release.

## Signed implementation sequence

Each row is one signed Conventional Commit. Each commit changes only the exact
one to three files in that row and leaves the repository buildable and green.
Every row before the product-baseline row implements and tests the mechanism
with the current development version. Those rows do not declare `1.0.0`.

| Conventional Commit | Exact files | Required result |
| --- | --- | --- |
| `docs(release): define the first stable release authority` | `docs/decisions/0047-first-one-zero-release.md`; `docs/decisions/README.md` | Accept and index this release mechanism without accepting product completeness. |
| `build(cargo): centralise artifact and client versions` | `Cargo.toml`; `crates/orna-artifact/Cargo.toml`; `crates/orna-client/Cargo.toml` | Add the current `0.1.0` workspace package version and make the first two crates inherit it. |
| `build(cargo): centralise compiler core and postgres versions` | `crates/orna-compiler/Cargo.toml`; `crates/orna-core/Cargo.toml`; `crates/orna-postgres/Cargo.toml` | Replace the three local product-version literals with workspace inheritance. |
| `build(cargo): centralise protocol server and standard versions` | `crates/orna-protocol/Cargo.toml`; `crates/orna-server/Cargo.toml`; `crates/orna-standard/Cargo.toml` | Replace the three local product-version literals with workspace inheritance. |
| `build(cargo): centralise syntax and system-test versions` | `crates/orna-syntax/Cargo.toml`; `crates/orna-system-tests/Cargo.toml` | Remove the final local product-version literals and prove that every workspace package resolves to `0.1.0`. |
| `feat(cli): report the canonical product version` | `crates/orna-server/src/main.rs`; `crates/orna-system-tests/tests/installed_product.rs` | Add exact `orna --version` output from `CARGO_PKG_VERSION` and prove the installed development package reports `0.1.0`. |
| `build(debian): separate development and release modes` | `packaging/debian/changelog`; `packaging/debian/rules`; `.github/workflows/debian-package.yml` | Add the exact unreleased development entry and require an explicit build mode. Change ordinary CI to call only `development-package`. Keep the current literal control and CI package identity for this row, but validate it against the changelog before build or test. Keep protected `1.0.0-1` closed. A dry-run, when added, validates only and does not change persistent state. |
| `build(debian): derive development package identity` | `packaging/debian/control`; `packaging/debian/rules` | Derive the Debian control version, source and binary package identity, and package filename from `dpkg-parsechangelog`. Remove the temporary literal identity checks only after the derived identity has replaced them. Keep the exact `0.1.0-1` development contract and the closed protected mode. |
| `build(debian): own dependency licence sources` | `LICENSE`; `packaging/debian/copyright`; `packaging/debian/dependency-licences.toml` | Define the exact Cargo.lock-keyed TOML record before adding inventory: common and conditional source fields, raw Cargo licence, normalised SPDX expression, one complete selected SPDX `OR` branch, selected-licence and notice inputs, canonical source URL, and exactly one holder branch. Derive the Debian source rendering from that URL. Generic MIT records require one of the closed four `{path, sha256}` identities and no notices. |
| `build(debian): generate release evidence` | `packaging/debian/release-evidence.sh`; `packaging/debian/orna.install`; `packaging/debian/rules` | Validate the complete locked closure, every input path and digest, holder XOR, slash-to-OR normalisation, complete SPDX branch selection, and the closed generic-MIT exception. For every registry record, require checksum-bound archive and VCS evidence, exact unnormalised manifest repository URL, and matching SBOM VCS and `packages[].downloadLocation` fields. Permit the closed `siphasher 1.0.3` `project_licence_text_input` only when it is the exact regular Orna project-root `LICENSE` at the accepted release commit. Reject automatic holder inference and any new generic-MIT input identity until an ADR amendment adds its `{path, sha256}` pair. Install deterministic notices, SPDX 2.3 SBOM, changelog, copyright, and licence evidence bound by the manifest. |
| `build(release): pin public signing policy` | `packaging/debian/release-policy.toml`; `packaging/debian/orna-archive-keyring.gpg`; `packaging/debian/publish-repository.sh` | Pin full public tag-signer and repository-key fingerprints, rotation and revocation state, keyring bytes, 14-day validity, and the immutable generation publisher interface without a secret or endpoint. |
| `test(release): prove protected publication` | `.github/workflows/debian-package.yml`; `.github/workflows/debian-release.yml`; `crates/orna-system-tests/scenarios/debian-release.sh` | Keep development artifacts non-production and use only non-production test keys and a synthetic candidate to prove mode separation, trust policy, signing, expiry, monotonic immutable generations, replay rejection, atomic promotion, tamper closure, predecessor closure, and no runtime Ed25519 authority. Production `1.0.0-1` remains closed. |
| `release(product): accept the 1.0 product baseline` | `docs/releases/1.0-product-acceptance.md` | After the complete product review, accept every supported claim, evidence mapping, and explicit deferral. This row is a mandatory gate, not an assumed result of the earlier rows. |
| `release(debian): declare 1.0.0-1` | `Cargo.toml`; `Cargo.lock`; `packaging/debian/changelog` | Only after the accepted product baseline, set the workspace version to `1.0.0`, update the locked workspace package identities, and replace the development entry with the released Bookworm `1.0.0-1` entry. The protected authority can then sign `v1.0.0`, reproduce, approve, sign the repository, and publish. |

The protected publication workflow and signing interface must remain outside
an ordinary CI trigger and must not add a repository-held private key or a
second package authority.

## Deferred surface

This decision does not accept or complete the Orna 1.0 product scope. It does
not select the final public language subset, compatibility term, support
period, service-level promise, backup policy, disaster recovery policy,
release cadence, repository retention policy, or a later package upgrade edge.

RPM packaging and repository publication are explicitly deferred. No `.rpm`,
RPM spec, DNF repository, RPM signing key, RPM signature, conversion from the
Debian package, or cross-format upgrade claim is part of the first release.
A later accepted decision must define RPM version mapping, payload parity,
maintainer scripts, signatures, repository metadata, clean-machine proof, and
upgrade rules before RPM work starts.

## Precedence

This decision narrows the first production release mechanics under work ADR
0019. It preserves ADR 0019's authenticated Debian package authority,
one-executable product boundary, manifest integrity bindings, complete
clean-machine gate, and empty first-release predecessor set.

It supersedes work ADR 0017 only where that partly superseded decision could
be read to require a detached Ed25519 signature, accepted-runtime record, or
separate signed PostgreSQL runtime for production. It preserves all other
accepted package transaction, lifecycle, failure, and data-transition rules
that work ADR 0019 retains.
