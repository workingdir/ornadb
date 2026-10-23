# ADR 0017: Orna Ships and Owns Its PostgreSQL Runtime

**Status:** Accepted; program-distribution model superseded by work ADR 0019

Work ADR 0019 replaces this record's separate PostgreSQL executable tree,
runtime-signing, publication, verification, and package-ingestion model. This
record's other host, instance, service, command, update, and package contracts
remain accepted as specified by work ADR 0019.

## Decision

The first production Orna distribution is one Debian 12 amd64 `.deb` package.
It installs one public executable:

```text
/usr/bin/orna
```

The package also installs a complete private PostgreSQL process bundle. It
does not depend on a host `postgres`, `psql`, PostgreSQL package, PostgreSQL
service, Docker service, container runtime, or first-start download.

The first runtime identity and root are exactly:

```text
postgresql-18.4-debian12-amd64-orna.1
/usr/lib/orna/postgresql/postgresql-18.4-debian12-amd64-orna.1
```

The private root contains `postgres`, `psql`, `initdb`, `pg_upgrade`, every
other PostgreSQL program that Orna needs, their private non-glibc shared
libraries, PostgreSQL support files, and signed Orna runtime evidence. The
package creates no public `postgres`, `psql`, or `pg_*` link. Orna starts each
private program by a verified absolute path. It never discovers one through
`PATH`.

PostgreSQL remains a separate operating-system process. `orna server run`
starts and supervises the postmaster. Orna does not link PostgreSQL into the
Rust process and does not treat `libpq` as an embedded server. This process
boundary preserves PostgreSQL's postmaster, worker-process, shared-memory,
write-ahead-log, and crash-recovery model.

The `.deb` file is the first complete production unit. A later single-file
launcher can contain and materialise the same verified private tree, but it
must preserve this runtime, instance, process, and verification contract.

## Command dispatch

After this decision, the complete host command set is the three exact command
shapes below. Each accepts no additional token or flag:

```text
orna server run
orna server upgrade
orna server backend-shell
```

Missing or invalid arguments write this exact text, including the final line
feed, to standard error and exit `2`:

```text
Usage:
  orna server run
  orna server upgrade
  orna server backend-shell
```

Command-shape dispatch occurs before terminal, account, package, instance, or
runtime checks. A later accepted source-check command can append its own line
to this global usage contract.

## Host ABI and dependency closure

The first production target is exactly Debian 12 amd64 on Linux kernel 6.1 or
later, with systemd and the Debian glibc 2.36 ABI. Linux `openat2` and the
required resolution flags are mandatory. `ENOSYS`, an unsupported resolve
flag, or an older kernel fails before runtime or instance use. The package can
use the Linux kernel, systemd service manager, declared Debian base-package
shell, and constrained glibc loader, core-library, and local account-file inputs
below. It cannot use a host PostgreSQL or an undeclared host library outside
the accepted Debian base ABI packages.

The `.deb` declares these base ABI dependencies:

```text
libc6:amd64 (>= 2.36)
libc6:amd64 (<< 2.37)
libc-bin (>= 2.36)
libc-bin (<< 2.37)
dash
libgcc-s1
```

Debian package installation and Debian's package trust enforce these
dependencies and bounds. Package-manager installation is the only supported
production installation path. At runtime, Orna does not query the dpkg
database. It verifies safe ancestors, links, owners, modes, amd64 ELF ABI, and
the permitted SONAME closure, but it does not pin host file digests or exact
package versions. Debian 12 security updates to these base packages remain
accepted. Every member below the private PostgreSQL root remains byte-pinned by
the signed manifest.

The public `/usr/bin/orna` ELF closure is separate from the private PostgreSQL
closure. It has interpreter `/lib64/ld-linux-x86-64.so.2`, no `RPATH` or
`RUNPATH`, and only these `DT_NEEDED` names:

```text
libc.so.6
libgcc_s.so.1
ld-linux-x86-64.so.2
```

Debian package trust and the declared `libgcc-s1` dependency supply
`libgcc_s.so.1`; Debian's normal loader, cache, and hwcaps rules can select its
compatible member. Package construction and the clean-machine gate inspect the
Orna binary and its live maps and reject another interpreter, needed name,
run path, or loaded non-base object.

Every dynamically linked bundled ELF executable uses this interpreter:

```text
/lib64/ld-linux-x86-64.so.2
```

Bundled shared libraries have no `PT_INTERP`. The manifest interpreter field
is present and exact for a dynamically linked executable and absent for a
shared library.

Debian's merged-`/usr` loader links are the only trusted structural link
exception. `/bin`, `/lib`, and `/lib64` must be the root-owned Debian links to
`usr/bin`, `usr/lib`, and `usr/lib64`. The interpreter above must resolve
through the root-owned Debian loader link to the regular multiarch glibc loader
under `/usr/lib/x86_64-linux-gnu`. Orna verifies the final loader path, owner,
mode, amd64 ELF ABI, and permitted SONAME closure without consulting package
metadata or fixing a file digest.

The only unbundled `DT_NEEDED` names permitted in the recursive PostgreSQL
runtime closure are these glibc components:

```text
libc.so.6
libdl.so.2
libm.so.6
libpthread.so.0
libresolv.so.2
librt.so.1
libutil.so.1
```

Orna parses only root-owned, non-linked, mode-`0644` `/etc/passwd` and
`/etc/group` files and requires one unambiguous local `orna` account and group.
It also parses the root-owned,
non-linked, mode-`0644` `/etc/nsswitch.conf`. The effective `passwd` and
`group` service lists must each begin with `files` under the default
`SUCCESS=return` action. A duplicate database line or an action override that
continues after a successful `files` lookup is rejected. Later services, such
as Debian's normal `systemd` entry, are permitted because the unique local
`orna` UID and GID succeed through the accepted glibc `files` service before
those services can run. Debian 12 glibc supplies that service through the
accepted libc ABI; Orna does not require or pin a separate
`libnss_files.so.2`. This also constrains PostgreSQL peer authentication,
which converts the Unix peer credential UID to a name through libc. DNS, LDAP,
NIS, SSSD, and a remote identity provider cannot supply the `orna` identity.
`TZ=UTC0` fixes PostgreSQL's process time zone without a host zoneinfo file.

PostgreSQL tools can call C `system()`. The first host closure therefore also
accepts Debian's root-owned `/bin` link to `usr/bin`, `/bin/sh` link to
`/usr/bin/dash`, and the final regular `dash` program with only its declared
glibc closure. The verifier requires the exact link targets, safe metadata, and
accepted amd64 ABI without fixing dash bytes or package version. No other host
shell is accepted. It also accepts the regular `/usr/bin/locale` executable
supplied through the declared `libc-bin` dependency. The verifier checks its
metadata and amd64 ABI and requires its recursive ELF closure to remain in the
accepted glibc ABI set, without fixing its bytes or package version. No other
host executable is accepted by a production Orna or PostgreSQL process.

Debian package maintenance has one separate, package-manager-only exception.
The new `preinst` may invoke the regular, non-linked, root-owned mode-`0755`
`/usr/bin/dpkg` through that absolute path, and only with `--compare-versions`
for the downgrade check defined below. Its `/usr` and `/usr/bin` ancestors must
have the exact trusted metadata in the path table below. Debian's package trust
is the authority for this executable during an already-running dpkg
transaction. It is not a production Orna or PostgreSQL runtime dependency, is
not added to either runtime ELF closure or process `PATH`, and cannot be
invoked by a public Orna command or a private PostgreSQL process. No other
external host executable beyond the already accepted `/bin/sh` interpreter is
accepted in a maintainer script; the private same-byte Orna package helper is
the only other executable it may invoke.

The private runtime bundles every other direct and transitive PostgreSQL
dependency. The first build bundles zlib but builds PostgreSQL and `psql`
without readline. Backend-shell therefore has no line editing in version 1
and uses no host terminfo database or terminal library. The build disables
unused OpenSSL, ICU, LDAP, PAM, GSSAPI, Kerberos, Bonjour, XML, XSLT, LZ4,
Zstandard, Perl, Python, Tcl, and systemd integration in PostgreSQL. A later
enabled feature requires a new runtime identity and complete signed closure.

Every PostgreSQL process receives one fixed private `PATH`:

```text
<verified-runtime>/bin:<verified-runtime>/libexec
```

Both directories are in the signed payload. `bin` contains only bundled
PostgreSQL tools. `libexec` contains only a signed, exact shell wrapper named
`locale`; it uses the accepted `/bin/sh` and immediately `exec`s the absolute
accepted `/usr/bin/locale`. Orna does not redistribute the glibc `locale`
executable. No host path appears in `PATH`. A direct `system()` call uses the
accepted Debian dash, whose command lookup remains limited to this private
path.

The build gives each private ELF member a relative run path into the verified
private library directory. The signed manifest records each optional ELF
interpreter, run path, and ordered `DT_NEEDED` list. Each non-glibc dependency
must resolve through that signed run path to a signed private member. A
permitted glibc SONAME can resolve through Debian's normal loader, cache, and
hwcaps selection as part of the trusted Debian 12 `libc6` ABI. Production
process environments contain no dynamic-loader override.

## Release provenance, identity, and signing

The first runtime uses the official PostgreSQL 18.4 source archive:

```text
https://ftp.postgresql.org/pub/source/v18.4/postgresql-18.4.tar.bz2
sha256 81a81ec695fb0c7901407defaa1d2f7973617154cf27ba74e3a7ab8e64436094
```

The build rejects any other archive bytes. The build recipe pins its Debian
12 amd64 build environment, compiler and linker inputs, configure flags,
patches, dependency sources, and environment. A release cannot use an
unrecorded local patch or library.

The private root contains these evidence files:

```text
orna-runtime-manifest.json
orna-runtime-manifest.sig
sbom.spdx.json
POSTGRESQL-LICENSE
```

The manifest records the runtime identity, target ABI, upstream source URL and
SHA-256 digest, build-recipe identity, configure flags, patches, toolchain,
dependency closure, and the type, owner, group, mode, size, SHA-256 digest, and
link target of every installed payload member. For each ELF member, it also
records the optional interpreter, run path, and ordered `DT_NEEDED` names. The
payload inventory excludes only the manifest and its detached signature. It
includes the software bill of materials (SBOM), PostgreSQL licence, executable
closure, libraries, and support files.

The inventory and byte digests cover the private root only. The manifest can
name the Debian base ABI packages and bounds as compatibility requirements, but
it does not record resolved host-library bytes, digests, or package versions.

The build recipe emits deterministic UTF-8 JSON with LF line endings, sorted
object fields, and payload entries sorted by path bytes. The SHA-256 digest of
that exact `orna-runtime-manifest.json` byte sequence is the sole private-tree
identity. There is no second normalised-tree digest. Two builds from the same
inputs must produce the same manifest bytes and the same payload bytes,
owners, groups, modes, and link targets.

The signature file is one raw 64-byte Ed25519 signature over the exact shipped
manifest bytes. It does not sign a parsed or re-encoded JSON value.

### Accepted-runtime record

Each checked-in accepted-runtime record is one UTF-8 TOML document with exactly
these root keys and no others. This is a schema illustration, not an accepted
record or a key.

```toml
format = 1
runtime_identity = "<canonical runtime identity>"
manifest_sha256 = "<64 lowercase hexadecimal characters>"
release_key_id = "ed25519-sha256:<64 lowercase hexadecimal characters>"
ed25519_public_key = "<64 lowercase hexadecimal characters>"
```

`format` is the integer `1`. Every other field is a string. The parser rejects
a missing, duplicate, or unknown key, a non-root value, or a value of another
type. `manifest_sha256` and `ed25519_public_key` each contain exactly 64 ASCII
lowercase hexadecimal characters. The public-key field decodes to the raw
RFC8032 32-byte Ed25519 public key. It is not PEM, OpenSSH, base64, or another
container or encoding.

`release_key_id` contains exactly the ASCII prefix `ed25519-sha256:` followed
by the 64 lowercase hexadecimal characters of the SHA-256 digest of those raw
32 public-key bytes. It is 79 ASCII bytes in total. A record has no archive
digest, candidate marker, signature, private key, path, seed, or URL. In
particular, the manifest digest is not an archive digest, and the binary, not
the record, marks one accepted runtime as its distribution candidate.

The record parser uses this exact order:

1. parse the TOML document as one root table;
2. require the exact five-key schema and value types;
3. require `format = 1`;
4. for the first record, require `runtime_identity` to be exactly
   `postgresql-18.4-debian12-amd64-orna.1`;
5. require the exact lowercase hexadecimal `manifest_sha256` value;
6. require and decode the exact lowercase hexadecimal
   `ed25519_public_key` value as 32 raw bytes; and
7. compute the public-key SHA-256 digest and require the exact canonical
   `release_key_id` value.

Only after this record validation does runtime verification select the record
by exact `runtime_identity`, compare the exact raw manifest digest, verify the
raw signature, and then parse the manifest JSON and verify the payload.

Before the first accepted record can be committed, the designated Orna release
authority generates its initial Ed25519 key only on a protected offline
software signer or hardware signer. The signer exports only the raw 32-byte
public key and one raw 64-byte Ed25519 possession proof. That proof is a
signature over the exact candidate manifest bytes, not a separate domain. For
the first acceptance, the authority receives the proposed five-field record,
exact candidate manifest bytes, and that signature. It applies the parser
order above, recomputes the manifest SHA-256 digest and compares it to
`manifest_sha256`, then verifies the signature against the raw public key. Only
then can it commit the proposed record as the public accepted record. The
signature is not a record field. The publish row can publish the same raw bytes
as the detached `orna-runtime-manifest.sig` signature.

The private key never leaves the protected signer. It never enters an online or
development workstation, source repository, ordinary continuous integration
system, Debian package, or installed host.

After this commit, the checked-in accepted-runtime record is the source of
truth for runtime identity, exact manifest SHA-256 digest, release-key
identifier, and Ed25519 public key. The current Orna binary marks exactly one
accepted record as its distribution candidate. The initial binary contains
only the PostgreSQL 18.4 record, which is therefore the candidate. The
runtime-verification crate embeds the records, and release and Debian packaging
consume the same records.

Pull-request and ordinary build continuous integration have no release private
key. They build the candidate twice and publish only the keyless candidate
archive, exact manifest bytes, and digest for review. The checked-in accepted
record then fixes the reviewed digest, key identifier, and public key.

The protected signer signs only the exact candidate manifest bytes that match
the proposed record digest. A protected publisher combines the unchanged
payload, manifest, and detached signature and publishes one immutable signed
runtime archive addressed by the manifest SHA-256 digest. A digest address can
never be replaced with different bytes.

The Debian build ingests only that protected signed archive. Before extraction
into the package staging tree, it verifies the archive address, accepted
record, exact manifest digest, Ed25519 signature, payload inventory, and
binary distribution-candidate identity. A direct keyless continuous-integration
candidate is not a Debian-package input. The package build and installed
runtime verifier reject a binary, accepted record, manifest, signature, or
private tree that does not form one exact accepted set.

For each accepted runtime, `/usr/bin/orna` matches the selected identity to
the embedded accepted record. It verifies the raw manifest digest and
signature before parsing JSON, and then verifies every parsed field and
payload member. A later signed Orna release can add a key through an
append-only accepted-key set. It cannot assign different manifest bytes or a
different key to an existing runtime identity.

The Debian repository signature authenticates normal package delivery. The
runtime signature adds an offline boundary for an installed or sideloaded
package. Verification rejects a missing or additional payload member, changed
metadata or bytes, an unsafe link, and a group-, other-, or service-writable
runtime member. A manifest-declared symbolic link must use the exact relative
target and resolve inside the private root.

The private tree is owned by `root:root`. Root owner-write permission is
permitted where the signed manifest records it. No directory or file can be
writable by the `orna` service account, the `orna` group, or other users. Only
programs declared executable by the manifest have execute bits. The signed
SBOM records dependency licence identifiers. The PostgreSQL licence remains
in the private evidence and normal Debian documentation tree. The package
installs every other required dependency notice in the Debian documentation
tree.

Verification needs no network access. Static accepted-record, signature,
private-tree, host-path, and ELF checks fail before a private program starts.
A live `/proc/<pid>/maps` check necessarily follows child execution. A map
outside the accepted closure makes Orna immediately kill and reap that child
and fail the operation before Orna reports readiness or issues a query. It does
not claim to prevent PostgreSQL startup, recovery, or other pre-check internal
work. Orna does not continue with a host program or another runtime.

## Service account and trusted paths

The first package defines one locked system account and one instance target:

```text
Unix account       orna:orna
login shell        /usr/sbin/nologin
instance name      default
configuration      /etc/orna/instances/default.toml
state root         /var/lib/orna/instances/default
package lock       /var/lib/orna/package.lock
package state      /var/lib/orna/package-state.toml
runtime root       /run/orna/default
ready record       /run/orna/default/ready
socket directory   /run/orna/default/postgres
database           orna
database role      orna_kernel
```

The systemd-sysusers package input creates a non-root system account with a
locked password, primary group `orna`, no supplementary groups, no login
shell, and no dynamic UID. Package and clean-machine tests inspect the local
account and shadow state and require the password to be locked. At runtime,
`orna server run`, `orna server upgrade`, and
`orna server backend-shell` resolve the local `orna` account and group and
require their effective UID and GID to equal those resolved values. They also
require the configured no-login shell and non-root UID. The supplementary GID
list must be empty or contain only duplicate entries equal to the resolved
primary GID. No other local account can use that primary GID, and the local
group member list must be empty or contain only duplicate `orna` entries. They
never trust a numeric UID from configuration. An account
lookup or identity mismatch fails before instance or runtime access. The
unprivileged commands do not claim that they can read `/etc/shadow`; package
installation and the production gate establish and test the password lock,
while the commands enforce the resolved account and process identity.

The first configuration is exactly:

```toml
format = 1
instance = "default"
```

An absent key, different instance name, unsupported format, or unknown key
fails closed. In particular, the file cannot select a PostgreSQL runtime. A
new instance uses the current binary's one distribution candidate. An existing
instance uses the active runtime identity in its durable instance manifest.
`orna server upgrade` uses the binary candidate. A later accepted
server-configuration decision can add keys with an explicit format rule.

Each production operation walks Orna-controlled paths component by component
with Linux `openat2`, `RESOLVE_BENEATH | RESOLVE_NO_SYMLINKS`, and
descriptor-based metadata checks. For the finite Debian structural-link
exceptions, it uses descriptor-relative metadata and link-target checks, then
opens the exact resolved target without following another unverified link. It
accepts these exact owners and modes:

| Path | Owner | Mode |
| --- | --- | --- |
| `/usr`, `/usr/bin`, `/usr/lib`, `/usr/lib64`, `/usr/lib/x86_64-linux-gnu`, `/usr/lib/orna`, `/usr/lib/orna/libexec`, `/usr/lib/orna/postgresql` | `root:root` | `0755` |
| `/bin`, `/lib`, and `/lib64` | `root:root` | exact Debian merged-`/usr` symbolic links |
| `/lib64/ld-linux-x86-64.so.2` | `root:root` | exact Debian loader symbolic link |
| final multiarch loader | `root:root` | `0755`, accepted amd64 glibc ABI and SONAME closure |
| `/bin/sh` | `root:root` | exact Debian symbolic link to `/usr/bin/dash` |
| `/usr/bin/dash` | `root:root` | `0755`, accepted amd64 ELF ABI and SONAME closure |
| `/usr/bin/locale` | `root:root` | `0755`, accepted amd64 glibc ABI and SONAME closure |
| `/usr/lib/orna/libexec/orna-package-maintenance` | `root:root` | `0755`, exact `/usr/bin/orna` bytes |
| accepted private runtime root | `root:root` | `0755` plus exact signed descendant modes |
| `/etc`, `/etc/orna`, `/etc/orna/instances` | `root:root` | `0755` |
| `/etc/passwd`, `/etc/group`, `/etc/nsswitch.conf` | `root:root` | `0644` |
| `/etc/orna/instances/default.toml` | `root:root` | `0644` |
| `/var`, `/var/lib`, `/var/lib/orna` | `root:root` | `0755` |
| `/var/lib/orna/package.lock` | `root:orna` | `0640` |
| `/var/lib/orna/package-state.toml` | `root:orna` | `0640` |
| `/var/lib/orna/instances` | `orna:orna` | `0700` |
| default state root and generation directories | `orna:orna` | `0700` |
| instance manifest, lock, and generated configuration files | `orna:orna` | `0600` |
| `/run` and `/run/orna` | `root:root` | `0755` |
| default runtime root and socket directory | `orna:orna` | `0700` |
| `/run/orna-upgrade` | `root:root` | `0755` |
| `/run/orna-upgrade/default` | `orna:orna` | `0700` |
| ready record | `orna:orna` | `0600` |

Above the private runtime root and throughout Orna-controlled configuration and
state, only the exact Debian loader, merged-`/usr`, and shell links listed above
can be symbolic links. Below the private runtime root, only a relative internal
link declared by the signed manifest is accepted. Every other Orna-controlled
component must be non-linked. No listed directory can be group- or
other-writable. The runtime verifier applies the signed owner, mode, and link
rules below the private runtime root. The state verifier rejects links and
unexpected file types throughout Orna-controlled state. PostgreSQL tablespaces
and user-selected data paths are not accepted.

The package creates `/var/lib/orna/instances` but not the `default` state
root. `orna server run` creates that root for a new instance. Systemd creates
the ephemeral runtime root for each service start. A present default state root
without a valid manifest is existing inconsistent state, not a new instance.

The state root and every PostgreSQL generation must use a local ext4 or XFS
filesystem. Orna checks the Linux filesystem type before initialisation,
upgrade, and start. It rejects NFS, CIFS, FUSE, overlay, tmpfs, Btrfs, and any
unknown type for durable state. The upgrade work directory and all data
generations stay on the accepted local state filesystem.

## Package-wide maintenance protocol

Package readiness and exclusion exist independently of instance state at these
exact paths:

```text
/var/lib/orna/package.lock
/var/lib/orna/package-state.toml
```

The lock is a non-linked, one-byte regular file containing LF, with link count
`1`, owner `root:orna`, and mode `0640`. The state is a non-linked regular file
with link count `1`, owner `root:orna`, and mode `0640`. Its only accepted
ready bytes are exact UTF-8 with one final line feed:

```toml
format = 1
state = "ready"
```

The package helper can persist these exact incomplete bytes:

```toml
format = 1
state = "incomplete"
```

A missing or unsafe package directory, lock, or state file, or state bytes
other than exact ready bytes, fails closed. The public command writes this
exact line to standard error and exits `1`:

```text
orna: package maintenance is incomplete
```

After command-shape and command-specific terminal and service-account checks,
`orna server run`, `orna server upgrade`, and
`orna server backend-shell` take the package lock before any instance lock.
Each opens the verified lock once for reading with `O_NOFOLLOW`, takes a
non-blocking POSIX `fcntl(F_SETLK)` read lock on byte range `[0,1)`, and then
requires exact ready state. A conflicting writer uses the same diagnostic as
non-ready state. Run and upgrade retain their sole package-lock descriptor for
their complete lifetimes. Backend-shell clears `FD_CLOEXEC` on that descriptor
immediately before process replacement so the verified private `psql` inherits
the read lock for the raw shell lifetime. Only after package readiness is
established can a command inspect or acquire the instance lock.

The package installs this private helper entry point:

```text
/usr/lib/orna/libexec/orna-package-maintenance
```

It contains the exact same accepted Orna binary bytes as `/usr/bin/orna` and is
owned by `root:root` with mode `0755`. The package proves byte identity. The
binary selects this entry point only for that private `argv[0]`, requires
effective UID `0`, and accepts exactly one private argument: `begin` or
`complete`. These arguments are not public `/usr/bin/orna` commands, do not
appear in public usage, and are not discoverable through `PATH`.

The helper performs every `openat2`, metadata, POSIX lock, atomic-write, and
`fsync` operation for package maintenance. `begin` opens the package lock once
for reading and writing, takes a non-blocking `F_SETLK` write lock on `[0,1)`,
and refuses while any server, upgrade, or backend-shell reader exists. Under
that lock, it accepts only exact ready or exact incomplete state and persists
exact incomplete bytes through a same-directory non-linked `root:orna`
mode-`0640` temporary file, file `fsync`, rename, and `/var/lib/orna` directory
`fsync`. It releases the lock only after incomplete state is durable.

`complete` first verifies the exact local `orna` account and group. It securely
creates or verifies `/var/lib/orna` as `root:root` mode `0755`, the
`root:orna` one-byte package lock, and the `root:orna` state path component by
component without following links. It synchronises every new file and
directory and its parent. It then takes the exclusive package lock, verifies
the installed public and private Orna binary identity, accepted records,
retained private roots, signatures, manifests, payloads, trusted paths, and
exact default configuration. Only after all verification succeeds does it
atomically persist exact ready state through the same temporary-file, file
`fsync`, rename, and parent-`fsync` sequence and release the lock. That durable
ready write is the package-protocol commit point. On an existing ready package,
an abort before `begin` leaves the prior ready state and package unchanged. A
failure after durable `begin` and before the verified ready commit leaves exact
incomplete state; an initial installation that has never committed ready state
can instead remain missing. Either condition stays fail-closed until a
successful package-manager repair. A crash after the durable ready commit
leaves a fully verified ready package even if dpkg bookkeeping must be replayed.

Maintainer scripts only validate their dpkg call shape, compare Debian versions
where required, and invoke this helper by its absolute private path. They do
not implement raw path, lock, atomic-file, or synchronisation syscalls
themselves.

## Durable instance manifest

The state root contains one lifetime instance lock, one durable
`instance.toml`, and numbered data generations:

```text
/var/lib/orna/instances/default/instance.toml
/var/lib/orna/instances/default/lock
/var/lib/orna/instances/default/generations/0000000000000001/data
```

The lock is a non-linked, one-byte regular file containing LF, with link count
`1`, owner `orna:orna`, and mode `0600`. Its creation includes file `fsync` and
parent-directory `fsync`. `orna server run` and `orna server upgrade` open it
once with `O_RDWR | O_CLOEXEC | O_NOFOLLOW`. They use Linux POSIX
`fcntl(F_SETLK)` to request a non-blocking whole-file write lock on byte range
`[0,1)`. Contention fails closed; Orna does not wait or use BSD `flock` or an
open-file-description lock.

`orna server run` retains that sole descriptor from before manifest read until
the direct postmaster has exited and supervision ends. `orna server upgrade`
requires the service to be stopped and retains the descriptor for the complete
maintenance operation. Neither operation reopens or closes the lock inode
elsewhere during its lifetime. A second run or upgrade cannot access the
instance concurrently.

The ready record stores the exact instance name and server PID. Backend-shell
opens the same verified instance-lock file only for inspection and uses
`fcntl(F_GETLK)` on byte range `[0,1)`. Where readiness depends on the lock,
the returned conflicting write lock must have `l_pid` equal to the recorded,
live server PID. An unlocked lock, stale PID, different holder PID, replaced
inode, changed link count, or invalid metadata makes readiness invalid.

The instance manifest records its format, instance name, current generation,
PostgreSQL system identifier and major version, exact runtime identity,
transition phase, every active or rollback-supported runtime and generation,
and `activation_committed`. The first generation number is
`0000000000000001`. A major upgrade allocates the next unused number. A
same-major runtime update keeps the current generation.

Before a manifest can refer to a newly created state root, `generations`
directory, generation directory, or data directory, Orna calls `fsync` on the
new directory and then on its parent. It repeats this from the data directory
through the generation, `generations`, state root, and existing
`/var/lib/orna/instances` parent as applicable. A directory-`fsync` failure
fails the operation before the reference is written.

Every instance-manifest change uses this exact durability sequence:

1. create a new non-linked temporary regular file in the state root;
2. write the complete new bytes and set owner `orna:orna` and mode `0600`;
3. call `fsync` on the temporary file;
4. rename it over `instance.toml` in the same directory; and
5. call `fsync` on the state-root directory.

Orna never synchronises the parent before rename as a substitute for the final
directory `fsync`. It never edits the live manifest in place and never infers
current state from the newest directory name.

An absent state root creates a new instance. A present state root with a
missing or invalid manifest, unsafe path, missing current generation,
mismatched PostgreSQL system identifier, unsupported major version, or
unknown transition fails closed. It does not create replacement storage.

## Cluster initialisation and private authentication

A new generation uses the candidate runtime's verified absolute `initdb` with
these exact arguments:

```text
--pgdata=<absolute-new-data-directory>
--username=orna_kernel
--encoding=UTF8
--locale-provider=builtin
--builtin-locale=PG_UNICODE_FAST
--data-checksums
--auth-local=peer
--auth-host=reject
--no-instructions
```

Orna does not use `--no-sync`. `initdb` completes its normal file and directory
synchronisation before Orna persists the generation in the instance manifest.
The empty target directory must be new, owned by `orna:orna`, mode `0700`, on
the accepted local filesystem, and below the state root. Orna never runs
`initdb` against a non-empty directory.

`orna_kernel` is the private PostgreSQL bootstrap superuser and remains the
private role used by Orna and backend-shell. This PostgreSQL superuser is
explicitly accepted because Orna owns migrations, protected schemas, and raw
host recovery. It is not a public Orna role and cannot connect through a
public pgwire endpoint.

Initial database creation uses the mode-`0700` private socket directory and a
separate bootstrap host-based authentication file with these exact UTF-8 bytes
and one final line feed:

```text
local postgres orna_kernel peer map=orna_default
local all all reject
```

Its identity map has these exact bytes and one final line feed:

```text
orna_default orna orna_kernel
```

The bootstrap postmaster listens on no TCP address. Orna connects only to the
`postgres` database created by `initdb` and runs the equivalent of
`CREATE DATABASE orna TEMPLATE template0`. It then stops that postmaster with
fast shutdown, installs the normal authentication files, and only then enters
normal recovery.

The normal postmaster listens on no TCP address. It creates only Unix socket
`.s.PGSQL.5432` under `/run/orna/default/postgres`. The directory and socket
have mode `0700` and owner `orna:orna`. Port `5432` is only the socket-file
suffix. TLS and password authentication are not part of this connection.

The normal host-based authentication file is exactly:

```text
local orna orna_kernel peer map=orna_default
local all all reject
```

The identity map is exactly:

```text
orna_default orna orna_kernel
```

Both normal files are exact UTF-8 bytes with one final line feed.

The Orna host and backend shell therefore connect to database `orna` as role
`orna_kernel` only when PostgreSQL reports the expected `orna` Unix peer.
Another Unix account, role, or database is rejected. Orna stores and passes no
PostgreSQL password.

## `orna server run`

`orna server run` is the foreground service command and accepts no additional
argument or flag. After command-shape and service-account checks, its first
state lock is the shared package lock. It then:

1. acquires and retains the shared package lock and verifies ready state;
2. acquires and retains the exclusive instance lock;
3. validates the current generation or initialises a new one;
4. recreates the private socket directory and removes stale readiness state;
5. verifies exact generated PostgreSQL configuration bytes;
6. starts the verified absolute `postgres` executable in the foreground;
7. waits for readiness through the private Unix socket;
8. runs private kernel bootstrap, standard-library installation, migrations,
   and active-revision recovery;
9. persists `activation_committed=true` and clears a completed activation
   transition through the exact manifest durability sequence;
10. atomically creates the matching mode-`0600` ready record; and
11. reports service readiness to systemd and supervises the direct postmaster.

The conservative `activation_committed=true` write occurs after all recovery
gates and before Orna reports ready. It is the exact major-generation rollback
boundary and does not claim that an application write occurred. A crash after
this durable marker but before ready cannot restore the old generation. This
rule can prohibit rollback when Orna never reported ready, but it cannot permit
rollback after activation was committed.

The ready record contains its format, instance name, server PID, postmaster
PID, generation, runtime identity, and instance-manifest SHA-256 digest.
Backend-shell accepts it only when every value matches live state, the server
PID exists, and the server still holds the instance lock. `server run` removes
the ready record before controlled shutdown or a reported postmaster failure.
If Orna dies before removal, the PID and lock checks reject the stale record, and systemd
removes the runtime directory when the service becomes inactive.

The PostgreSQL child starts from an empty environment. Orna supplies only
`LANG=C.UTF-8`, `LC_ALL=C.UTF-8`, `TZ=UTC0`, and the fixed private `PATH`
defined above. It supplies no `HOME`, `PG*`, dynamic-loader override, service
definition, password file, host path, or inherited application environment.

The child receives the absolute data directory and command-line `-c` values
for all security-critical host facts. These values fix the configuration,
authentication, and identity-map file paths, `listen_addresses=''`,
`port=5432`, socket directory, socket mode `0700`, `ssl=off`,
`allow_alter_system=off`, empty local, session, and shared preload library
lists, `archive_mode=off`, `archive_command=''`, and `archive_library=''`.
An instance file cannot override these command-line values.

Orna creates `postgresql.auto.conf` as an empty regular file. Before every
start, it requires the file to remain empty, non-linked, owned by `orna:orna`,
and mode `0600`. It compares the generated main, authentication, and identity
map files with their exact expected bytes. A difference fails closed.

`allow_alter_system=off` reduces accidental changes. It is not the security
boundary. The empty auto-configuration check, exact generated files,
command-line overrides, private socket, service account, trusted paths, and
file modes form the first host boundary. A host administrator or raw
backend-shell operator remains trusted and can damage private state.

The Debian unit uses `Type=notify`, `NotifyAccess=main`, `KillMode=mixed`,
`KillSignal=SIGINT`, `TimeoutStopSec=90s`,
`SendSIGKILL=yes`, `Restart=on-failure`, `RestartSec=5s`,
`StartLimitIntervalSec=60s`, and `StartLimitBurst=3`. Orna does not use
`pg_ctl` to daemonise PostgreSQL.

On unit stop, `KillMode=mixed` sends the initial `SIGINT` only to the Orna main
process. Orna removes readiness state, sends `SIGINT` to the direct postmaster
for PostgreSQL fast shutdown, and waits for that direct child for at most 60
seconds. Orna does not claim to reap every PostgreSQL descendant or process
group. The systemd cgroup owns the complete production process closure. If
Orna dies or its bounded wait ends with descendants present, systemd sends
`SIGKILL` to the remaining cgroup at the unit timeout.

If the direct postmaster exits unexpectedly, Orna removes readiness state,
waits for that child, and exits non-zero. It has no in-process restart loop.
Systemd stops any remaining cgroup process and applies the rate-limited
restart. Every restart re-enters signed-runtime verification, PostgreSQL crash
recovery, migrations, standard verification, and active Orna recovery before
it can report ready.

Orna never invokes `pg_resetwal` directly or as automatic repair. A readiness
timeout, PostgreSQL recovery error, kernel migration error, standard-library
error, or active-revision recovery error causes fast shutdown and a non-zero
Orna exit. The original durable state remains for explicit recovery.

## Backend shell

The operator command remains exactly:

```text
orna server backend-shell
```

It remains a local, interactive, host-only escape hatch. All three standard
streams must be terminals. It remains absent from Orna source, functions,
scripts, artefacts, and the public protocol. It does not start, stop,
bootstrap, migrate, repair, or change instance state before attachment.

Backend-shell requires the `orna` service-account identity and all
trusted-path rules above. It retains the shared package lock, requires exact
ready package state, and only then inspects the instance. It can attach only to
an already-ready `orna server run` host. It validates the fixed configuration,
durable manifest, signed runtime, ready record, live server PID, and socket
path without writing any file. Its instance-lock `F_GETLK` result must identify
the same live server PID as the lock holder. It never creates or changes
`activation_committed`; a ready host has already persisted that value as true.

The command directly replaces Orna with this exact verified private program
and argument sequence:

```text
/usr/lib/orna/postgresql/<verified-runtime>/bin/psql
--no-psqlrc
--no-password
--host=/run/orna/default/postgres
--port=5432
--username=orna_kernel
--dbname=orna
```

No command shell interprets the arguments. The command clears its environment.
It supplies `LANG=C.UTF-8`, `LC_ALL=C.UTF-8`, an empty `PSQL_PAGER`, and the
caller's non-empty `TERM`. It supplies `TERM=dumb` when the caller has no
non-empty value. It also supplies `TZ=UTC0` and the fixed private `PATH`. It
supplies no `HOME`, URL, password, `PG*` variable, host path, service file,
passfile, TLS input, or inherited psql option.

Once replacement succeeds, native `psql` output, exit status, signals,
meta-commands, and operator SQL remain raw PostgreSQL administration behaviour.
Version 1 has no readline line editing. The `psql` `\!` meta-command can invoke
the accepted Debian dash, with command lookup restricted to the private path;
this is trusted raw host administration, not a public Orna capability. The
trusted operator can change private state after attachment. Normal Orna
recovery must validate that state at the next service start.

The global command-shape rule above runs before every other boundary.

The remaining checks occur in this exact order: terminal, service-account
identity, package readiness, absent instance, instance configuration and path
state, bundled runtime, and process replacement. They use these exact lines and
exit `1`:

```text
orna: backend-shell must be run in an interactive terminal
orna: backend-shell must run as the orna service account
orna: package maintenance is incomplete
orna: the default Orna instance is not installed
orna: the default Orna instance is invalid
orna: the bundled PostgreSQL runtime is not valid
orna: could not start the bundled psql
```

An account lookup, effective-ID, or account-contract failure uses the
service-account line. A missing, unsafe, locked-for-write, or non-ready package
protocol uses the package-maintenance line. An absent fixed configuration or
absent default state root uses the `not installed` line. An invalid
configuration or manifest, unsafe owner or mode, symbolic link, non-local
filesystem, absent or mismatched ready record, dead server PID, unheld lifetime
lock, or unsafe socket path uses the `instance is invalid` line. An
accepted-record, signature, manifest digest, payload, ABI, or
instance-to-runtime mismatch uses the runtime line. A failure to replace the
process uses the final line. After successful replacement, connection and
peer-authentication failures are native `psql` diagnostics.

## `orna server upgrade`

The maintenance command is exactly:

```text
orna server upgrade
```

It accepts no flag or argument and operates only on the default instance. The
global command-shape rule runs first. The command then requires the resolved
`orna` service-account identity, all trusted-path and local-filesystem rules,
and a current binary whose embedded accepted-runtime set contains the installed
current runtime and marks exactly one accepted runtime as its distribution
candidate.

Upgrade already retains the shared package lock and exact ready package state.
The systemd service must also be stopped. Upgrade uses instance `F_GETLK` and
requires the lifetime range to be unlocked. It then acquires the exact instance
`F_SETLK` lock before it reads or changes the instance manifest. It also
requires no valid ready record, no live PID from the last ready record or
`postmaster.pid`, and no private normal-service socket. It holds both lock
descriptors until every candidate postmaster and upgrade program has exited and
the final manifest change is durable. It does not stop a running service
itself.

The private upgrade paths are exactly:

```text
/run/orna-upgrade/default
/var/lib/orna/instances/default/upgrade/<transition-id>
```

The package tmpfiles definition keeps `/run/orna-upgrade` and its `default`
child available while the normal service is inactive. Upgrade requires that
tmpfiles-created child to exist as a non-linked `orna:orna` mode-`0700`
directory and fails closed if it is absent or different; the unprivileged
command does not recreate it. The work directory is
a new `orna:orna` mode-`0700` directory on the same accepted local filesystem
as the generations. It synchronises that directory and its parent before use.
Upgrade starts every program by its verified absolute path with an empty
environment and the same fixed private path, locale, time-zone, loader,
no-TCP, preload, archive, and auto-configuration rules as server run.

Upgrade postmasters use a separate host-based authentication file with these
exact UTF-8 bytes and one final line feed:

```text
local all orna_kernel peer map=orna_upgrade
local all all reject
```

Their identity map has these exact bytes and one final line feed:

```text
orna_upgrade orna orna_kernel
```

This map allows only the `orna` operating-system peer to become `orna_kernel`
across the databases that `pg_upgrade` must inspect. It is used only on the
mode-`0700` upgrade socket and is never installed as the normal authentication
file.

The instance manifest records the previous runtime and generation, candidate
runtime and generation, transition identifier, transition kind, exact phase,
and rollback eligibility before a candidate program can change data. Every
phase change uses the exact temporary-file, file-`fsync`, rename, and
directory-`fsync` sequence.

When the binary distribution candidate equals the current runtime and no
transition is active, upgrade verifies the instance and runtime and exits
successfully without starting PostgreSQL or changing the manifest.

### Same-major runtime update

When the current and candidate PostgreSQL major versions are equal, upgrade
keeps the current data generation. It performs this sequence:

1. verify both complete private runtime trees and their accepted records;
2. use each private `pg_controldata` to verify the generation system
   identifier, catalogue version, checksum state, and shutdown state;
3. persist a `same_major_candidate_may_open` transition before the candidate
   postmaster can open the generation;
4. start the candidate postmaster on the private upgrade socket;
5. wait for PostgreSQL readiness and run Orna migrations, standard
   verification, and active-revision recovery;
6. stop the candidate with fast shutdown and wait for its direct child;
7. persist the candidate as the current runtime with
   `activation_committed=false` and phase `activation_pending`; and
8. remove the upgrade socket and work directory.

The old runtime can be restored only before phase
`same_major_candidate_may_open` is durable. After that phase is durable, Orna
assumes that the candidate opened the shared generation even if interruption
occurred before process creation. It never starts the older runtime against
that generation automatically. A failure requires forward recovery with the
candidate or a later accepted runtime. This conservative rule does not assume
reverse minor-version compatibility.

The next `orna server run` rechecks and recovers the candidate. It persists
`activation_committed=true`, clears the completed transition, reports ready, and
then supervises. If this recovery fails, the candidate transition remains for
forward maintenance and no older same-major runtime is selected.

### Major-version update

When PostgreSQL major versions differ, upgrade allocates a new generation and
uses the candidate runtime's exact `initdb` contract above. It then invokes the
candidate's verified private `pg_upgrade` from the private upgrade work
directory. The check invocation is equivalent to this fixed shape:

```text
<candidate-root>/bin/pg_upgrade
--check
--old-bindir=<current-root>/bin
--new-bindir=<candidate-root>/bin
--old-datadir=<current-generation>/data
--new-datadir=<candidate-generation>/data
--socketdir=/run/orna-upgrade/default
--old-port=50432
--new-port=50433
--username=orna_kernel
--old-options=<fixed-private-old-postmaster-options>
--new-options=<fixed-private-new-postmaster-options>
--sync-method=fsync
```

The fixed postmaster options select the private upgrade socket, empty
`listen_addresses`, exact authentication files, empty preload lists, disabled
archive settings, and empty verified `postgresql.auto.conf` files. If the
check succeeds, Orna invokes the same exact command without `--check` and with
`--copy`. It never uses `--link`, `--clone`, or `--copy-file-range`.

Orna inspects `pg_upgrade` output and its generated files before a generation
switch. If it emits a required rebuild or reindex script, or a warning that
requires any post-upgrade action, the candidate is rejected. General execution
of post-upgrade scripts is deferred. Orna never invokes `pg_resetwal` directly
or uses it as a repair mechanism. Verified internal use of PostgreSQL helpers,
including `pg_upgrade` invoking its bundled `pg_resetwal`, is part of the
accepted `pg_upgrade` operation.

After `pg_upgrade --copy`, Orna starts the candidate generation only on the
private upgrade socket. It verifies PostgreSQL state, data checksums, Orna
migrations, standard-library state, and active-revision recovery, and then
stops it with fast shutdown. Only then does Orna atomically select the new
generation and candidate runtime with `activation_committed=false` and phase
`activation_pending`.

The old and new generations share no files. The old generation remains a
rollback candidate only until `activation_committed=true` becomes durable. It
is not promised to remain byte-for-byte unchanged: `pg_upgrade --check` can
start the old postmaster, and PostgreSQL can perform normal recovery and
maintenance writes. Its logical state remains the pre-upgrade rollback point.

The next `orna server run` recovers the selected new generation. Before it
reports ready, it persists `activation_committed=true` and clears rollback
eligibility. After that durable change, automatic and manual selection of the
old generation is forbidden. The old and new generations can then have
different durable histories even if no public operation exists yet.

### Interruption and re-entry

The no-argument upgrade command uses the durable phase to re-enter an
interrupted transition:

* Before the current-generation switch, the old generation remains current.
  Upgrade validates it, removes only a recorded incomplete new generation,
  creates a new unused generation, and repeats check and copy. It never
  modifies or deletes the current generation as cleanup.
* After a major-generation switch with `activation_committed=false`, upgrade
  first attempts candidate validation and recovery again. If the candidate
  cannot recover, it can atomically restore the recorded old generation and
  runtime because `--copy` shares no files and the rollback boundary was not
  committed. It records the failed candidate and does not delete either
  generation.
* After `activation_committed=true`, no phase permits rollback. Upgrade can only
  validate the current generation or move forwards to another accepted
  candidate. This includes a crash after that marker but before ready.
* After `same_major_candidate_may_open`, no phase permits the older runtime to
  reopen the shared generation. Upgrade resumes or repairs forwards.

An unknown phase, missing referenced runtime, unsafe work directory, live
unrecorded process, or inconsistent generation fails closed. Upgrade never
invents a transition from directory timestamps and never deletes a generation
that the instance manifest names.

## Debian package retention and removal

Version 1 package history is append-only. Every newer Orna `.deb` contains
every immutable private runtime root contained by every earlier accepted
`.deb`, whether or not an instance currently names it. An update installs new
roots beside all earlier roots and never replaces or removes a member of an
existing runtime identity. Automated cleanup of old runtime roots is deferred.

Package construction extracts its own staged `.deb` and proves that the one
`/usr/bin/orna`, its private helper copy, all accepted-runtime records,
signatures, manifests, and private trees match exactly. Installation cannot
silently pair an older Orna binary with a newer helper or unaccepted tree.
Runtime verification repeats the private-tree check before every private
program execution.

For an upgrade, the old package `prerm upgrade` runs before the new package
`preinst upgrade`. The old `prerm` skips `begin` solely because its exact dpkg
call shape is `upgrade`. The new `preinst` accepts only the exact
`new-preinst upgrade <old-version> <new-version>` shape and compares the two
arguments using exactly
`/usr/bin/dpkg --compare-versions <new-version> lt <old-version>`. Shape
validation precedes comparison. If the comparison is true, it writes exactly
`orna: package downgrade is not supported` plus LF to standard error and exits
`1` before invoking the helper or changing package-protocol state. Equal or
newer versions proceed; a comparison error fails closed before `begin`. Only
then does `preinst` invoke the still-installed absolute private helper with
`begin` before unpack. Orna does not support a package downgrade. For removal
or purge, the installed `prerm` invokes that same helper with `begin` before
package-owned files can disappear. The helper requires the unit to be
inactive, takes the exclusive package lock, and persists incomplete state
before returning. Lock conflict with a running server, manual upgrade, or
inherited backend shell refuses the dpkg action.

No old private helper exists for either of these accepted new-package install
forms:

```text
new-preinst install
new-preinst install <old-version> <new-version>
```

The first form covers an initial installation and reinstall after purge. Dpkg
uses the second form when reinstalling from Config-Files after removal. The
first installation has missing package state; removal and purge leave exact
incomplete state. For only these helper-less install forms, `preinst` can
proceed to unpack without performing a package-protocol filesystem operation.
Any other helper-less call shape fails closed.

During unpack, the new public command remains fail-closed on missing or
incomplete state. The new `postinst` invokes the newly unpacked private helper
with `complete`. That helper creates the protocol files if needed, verifies the
complete installed package and configuration, and persists ready state. An
update, repair, reinstall-after-removal, or reinstall-after-purge reaches ready
through the same `complete` operation. An abort before `begin` on an existing
package leaves its prior ready state and bytes unchanged. A failure after
durable `begin` but before the verified ready commit leaves incomplete state;
only an initial installation with no earlier ready commit can remain missing.
A crash after the durable ready commit leaves the fully verified ready state in
place even when dpkg bookkeeping subsequently needs replay.

The package manager does not run `initdb` or `pg_upgrade`, select a runtime or
generation, change an instance manifest, or perform another data transition.
It does not auto-start or restart the service. For an existing instance, the
operator completes package repair, runs `orna server upgrade`, and then starts
`orna server run`; a fresh installation can start `orna server run` only after
successful `postinst`.

The package installs the exact inspectable
`packaging/debian/default.toml` bytes as
`/etc/orna/instances/default.toml`. The file contains no runtime selection, so
Debian configuration-file preservation cannot select a stale candidate. Any
local byte divergence fails the exact configuration check.

Package removal and purge require successful `begin`, leave exact incomplete
package state, and then remove package-owned programs. They do not delete the
persistent package lock, package state, `/var/lib/orna` instance state, or any
data generation. Reinstallation must provide the runtime named by the retained
instance manifest and complete package verification before any command can
start.

## Development-only external backend

Repository development and integration tests can inject an external
PostgreSQL connection through an explicit test-only adapter. Docker Compose
can provide that backend for live tests. This adapter is behind test code or a
non-production Cargo feature that the Debian release build explicitly rejects.

The production binary does not read `ORNA_SERVER_POSTGRES_URL`,
`DATABASE_URL`, inherited `PG*` variables, Compose files, or a host PostgreSQL
service. A development adapter cannot become a fallback when bundled-runtime
verification, instance state, or private startup fails. Tests exercise the
two seams separately.

The first development tracer verifies the upstream source digest, builds the
private tree twice, compares exact manifests and payloads, and runs the
candidate absolute `postgres --version` and `psql --version` paths with the
fixed private `PATH` and no inherited host path. It does not claim production
signing, installation, service ownership, or clean-machine distribution.

The first production tracer additionally requires the reviewed
accepted-runtime record, protected signature, runtime verifier, complete
Debian package, systemd lifecycle, default-instance bootstrap, and the
network-disabled clean-machine gate below.

## Required proof

The release and implementation must prove:

* the official PostgreSQL 18.4 archive has the exact accepted SHA-256 digest,
  and one changed source byte fails before compilation;
* two isolated builds produce identical manifest bytes, payload bytes, owners,
  groups, modes, and link targets, with no second tree identity;
* accepted-runtime record parsing accepts only the five declared TOML keys,
  `format = 1`, the exact first runtime identity
  `postgresql-18.4-debian12-amd64-orna.1` before digest or key validation,
  lowercase 64-character manifest and raw-public-key hex, and a
  `release_key_id` derived from the raw 32-byte public key; it rejects missing,
  duplicate, unknown, differently typed, wrong-runtime-identity, PEM, OpenSSH,
  base64, archive-digest, candidate, signature, private-key, path, seed, and
  URL inputs before runtime selection;
* the initial release key is generated only by the designated release authority
  on a protected offline software signer or hardware signer; only its raw
  public key and raw 64-byte signature over the exact candidate manifest leave
  that signer; the authority receives the proposed five-field record and
  manifest, recomputes and compares the digest, verifies that signature against
  the raw public key, and only then commits the public record; the signature
  remains outside that record and can become the detached published signature;
  and a private key never leaves the signer or enters an online or development
  workstation, source control, ordinary continuous integration, packages, or
  installed hosts;
* pull-request and ordinary build jobs contain no signing key, and the
  protected signer refuses manifest bytes whose digest differs from the
  reviewed accepted-runtime record; the protected publisher preserves the
  exact bytes at their immutable manifest-digest address; and Debian ingestion
  rejects a changed archive, address, record, manifest, signature, or
  candidate;
* raw-manifest digest verification and Ed25519 verification occur before JSON
  parsing, and a changed runtime identity, accepted record, signature,
  manifest, payload, SBOM, licence, or support file fails closed;
* static ELF inspection accepts only the fixed interpreter, signed private run
  paths, signed non-glibc dependencies, and permitted glibc SONAMEs; live
  PostgreSQL-process `/proc/<pid>/maps` inspection rejects every loaded
  non-glibc object outside the signed private root and accepts base glibc
  objects through the declared Debian ABI rather than byte identity; a live-map
  rejection kills and reaps the child before Orna readiness or an Orna-issued
  query, without claiming to prevent PostgreSQL recovery work;
* `/usr/bin/orna` has the fixed separate interpreter and three-name
  `DT_NEEDED` closure, has no run path, and its live maps contain no non-base
  host object beyond the declared `libgcc_s.so.1`;
* the host is Debian 12 amd64 with kernel 6.1 or later; package-manager
  installation enforces both `>= 2.36` and `< 2.37` libc bounds; and runtime
  checks every
  required `openat2` flag, loader link, merged-`/usr` link, host-member owner,
  mode, amd64 ABI, and SONAME closure without querying dpkg or pinning a
  security-update version or digest; `ENOSYS` and an unsupported flag fail
  before instance or runtime use;
* package proof verifies that `/usr/bin/dpkg` is one root-owned, non-linked,
  mode-`0755` regular file below the trusted `/usr` and `/usr/bin` ancestors;
  only new `preinst` invokes it, by its absolute path and solely as
  `--compare-versions <new-version> lt <old-version>` before `begin`; no
  production Orna or PostgreSQL runtime path, ELF closure, or process invokes
  it;
* account files contain one local `orna` identity and the effective
  `nsswitch.conf` `passwd` and `group` rules begin with `files` and return on
  success, so PostgreSQL peer-name lookup cannot reach a later identity source;
* a minimal Debian 12 host supplies only the accepted glibc and dash closures,
  no non-glibc PostgreSQL dependency, host PostgreSQL program, package,
  service, terminal library, or terminfo database; only signed private
  PostgreSQL tools plus the signed `locale` wrapper are reachable through the
  fixed private path;
* root owner-write mode is accepted only when recorded, while service-, group-,
  or other-write access to the private runtime is rejected;
* trusted `/usr`, `/usr/bin`, `/usr/lib`, `/usr/lib64`,
  `/usr/lib/x86_64-linux-gnu`, `/usr/lib/orna`, `/usr/lib/orna/libexec`,
  private-runtime, `/etc`, `/var`, `/run`, and upgrade-runtime ancestors have
  the exact owners and modes, and every unaccepted link, wrong owner, wrong
  mode, wrong type, or replacement fails before use;
* the package creates a locked local `orna` system account, and run, upgrade,
  and backend-shell reject an unresolved account or mismatched effective UID
  or GID without reading shadow state or describing their runtime check as a
  password-lock check;
* the instance lock is one owner-`orna`, mode-`0600`, link-count-`1` regular inode;
  `F_SETLK` excludes another server or upgrade for byte range `[0,1)`; the
  holder retains its sole descriptor; and `F_GETLK` distinguishes the matching
  live ready PID from an unlocked, stale, replaced, or differently held lock;
* the root-owned package lock and state have their exact independent metadata
  and bytes; run, upgrade, and backend-shell retain shared package locks before
  instance access; backend-shell carries its descriptor through `exec`; and a
  missing, unsafe, writer-locked, or non-ready protocol produces the exact
  package diagnostic;
* private helper `begin` excludes every package reader and durably commits
  incomplete state before update unpack or removal, while `complete` verifies
  the installed package and durably commits ready state under the exclusive
  lock;
* upgrade ordering makes old `prerm upgrade` skip mutation before new
  `preinst upgrade` validates its exact shape, rejects a Debian-version
  downgrade with the exact diagnostic and exit `1` before `begin`, and invokes
  existing-helper `begin` only for an equal or newer version; removal `prerm`
  invokes `begin`; and no other maintainer-script order can expose ready state
  during package replacement;
* only the root-only same-byte private helper accepts exact `begin` and
  `complete` entry points; the public binary keeps the global three-command
  usage; and maintainer scripts invoke the helper without implementing path,
  lock, atomic-write, or `fsync` operations;
* first-install unpack starts with missing state, so the unpacked public command
  remains fail-closed until the private helper securely creates and
  synchronises the protocol and completes verification;
* ext4 and XFS state roots are accepted, while each unsupported local, network,
  userspace, overlay, and memory filesystem is rejected;
* instance-manifest fault injection at every write, file-`fsync`, rename, and
  parent-`fsync` boundary recovers either the complete old bytes or complete
  new bytes and never selects an unrecorded generation; directory-creation
  fault injection proves every new state and generation directory and parent
  is durable before a manifest can refer to it;
* exact `initdb` arguments create UTF-8, built-in `PG_UNICODE_FAST`,
  checksum-enabled storage with bootstrap superuser `orna_kernel`, normal
  synchronisation, and no TCP authentication; exact bootstrap bytes connect
  only to `postgres`, create `orna` from `template0`, fast-stop, and install the
  exact normal authentication bytes;
* the normal socket, peer map, database, role, permissions, final line feeds,
  and rejection rows are exact, and another peer, database, or role cannot
  connect;
* hostile environment, `PATH`, fake host programs, `.pgpass`, service files,
  Compose files, loader variables, current directory, and time-zone files
  cannot change a production runtime or connection;
* global missing, invalid, and extra command tokens produce the exact four-line,
  three-command usage body, final line feed, and exit `2` before every other
  check;
* changed generated configuration, a non-empty `postgresql.auto.conf`, preload
  request, archive setting, or conflicting instance option fails before
  service readiness;
* server run retains the instance lock, completes bootstrap, standard install,
  migrations, and recovery, durably sets `activation_committed=true`, creates
  the matching instance-and-PID ready record, and only then reports ready to
  systemd; a crash after the marker but before readiness cannot roll back;
* normal stop sends fast shutdown and waits only for the direct postmaster,
  while `KillMode=mixed` initially signals only Orna and the bounded unit
  timeout sends `SIGKILL` to a remaining cgroup;
* an unexpected postmaster exit removes readiness, makes Orna exit non-zero,
  leaves no in-process restart, and causes a rate-limited complete recovery on
  the next systemd start;
* backend-shell follows the exact global usage, terminal, effective-account,
  package-readiness, absent, invalid-instance, invalid-runtime, and exec
  diagnostic precedence; uses `--no-psqlrc` and `--no-password`; requires an
  already-ready locked host; supplies only the fixed private path; exposes raw
  no-readline `psql` and its trusted dash `\!` behaviour; and changes no
  instance file before replacement;
* same-major upgrade never allocates a generation, never returns to the old
  runtime after the candidate-may-open phase, and re-enters interruption only
  in the recorded forward direction;
* major upgrade uses new checksum-enabled initialisation, exact old and new
  private paths, package-managed upgrade socket, exact upgrade-only peer files,
  `pg_upgrade --check`, then `pg_upgrade --copy`, with no shared file between
  generations; a required rebuild, reindex, or other post-upgrade action
  rejects the candidate before switch;
* interruption before a major switch keeps the old generation current,
  interruption after a switch but before activation commit permits the
  recorded copy-based rollback, and `activation_committed=true` forbids every
  old-generation rollback, including before readiness;
* every newer `.deb` retains every earlier accepted private runtime root,
  helper `begin` refuses an active service or conflicting reader, package work
  performs no data transition, and install leaves the service stopped;
* fault injection proves that abort before `begin` preserves the old ready
  package, initial-install failure before its first ready commit can remain
  missing, failure after durable `begin` and before the verified ready commit
  remains incomplete, and a crash after durable `complete` retains fully
  verified ready state even if dpkg bookkeeping needs replay; missing or
  incomplete state blocks run, upgrade, and backend-shell;
* helper-less `new-preinst install` covers first install and reinstall after
  purge, helper-less `new-preinst install <old-version> <new-version>` covers
  Config-Files reinstall after removal, and each new `postinst` must complete
  verification before commands reopen;
* package construction rejects a public/private Orna byte mismatch or an
  accepted-runtime record, manifest, signature, and payload mismatch; and
* package removal and purge require `begin`, leave incomplete package state,
  and preserve the package protocol, all instance state, and generations.

The production release gate uses a minimal Debian 12 amd64 virtual machine with
kernel 6.1 or later and ext4 or XFS. It proves required `openat2` resolution
support and the declared Debian base ABI plus accepted glibc name-service,
dash, locale, loader-link, account, and `nsswitch.conf` boundary. It stages the
local `.deb`, disables
network access, and only then installs it. The machine has no host `postgres`,
`psql`, PostgreSQL package or service, Docker, Podman, or container socket. The
test adds hostile programs to `PATH` and hostile PostgreSQL, loader,
home-directory, and current-directory inputs.

The gate runs `orna server run` under the packaged systemd unit, proves that
kernel bootstrap and the exact standard revision became durable, stops the
unit, starts it again, and proves that recovery retained the same verified
kernel and standard identities. It then enters backend-shell through a
pseudo-terminal and inspects that private state. The gate does not invoke an
Orna application command that this decision has not accepted. It must pass
without network access, another package installation, or a service outside the
Orna package.

## Implementation sequence

Each row is one buildable, reviewable Conventional Commit. Each commit changes
only the exact one to three files listed in that row.

Work ADR 0016's `feat(server): open standard-backed databases` row is a
prerequisite to the server rows below. It promotes `orna-kernel-postgres` from
a development-only dependency to the normal production dependency used for
bootstrap, migration, standard installation, and recovery. The dependency row
below adds the remaining production host dependencies and does not duplicate
that earlier ownership.

| Conventional Commit | Exact files | Required result |
| --- | --- | --- |
| `docs(architecture): own the PostgreSQL runtime` | `docs/decisions/0017-bundled-postgresql-runtime.md`; `docs/decisions/README.md` | Accept the production dependency, trust, lifecycle, update, and proof contract. |
| `build(postgres): produce a deterministic runtime` | `packaging/postgresql/runtime-build.toml`; `packaging/postgresql/build-runtime.sh`; `.github/workflows/postgresql-runtime.yml` | Verify source and dependency inputs, build twice without a key, and emit identical candidate payload and manifest bytes. |
| `build(postgres): accept the first signed runtime` | `packaging/postgresql/accepted-runtime-18.4.toml` | The authority receives the proposed strict five-key public record, exact candidate manifest bytes, and raw detached signature. It requires the exact first runtime identity before digest or key checks, recomputes and compares the manifest digest, verifies the signature with the raw public key, and only then commits format, runtime identity, manifest digest, derived release-key identifier, and raw public key. Do not commit an archive digest, candidate marker, signature, private key, path, seed, or URL. |
| `release(postgres): publish the signed runtime` | `packaging/postgresql/publish-runtime.sh`; `.github/workflows/postgresql-release.yml` | Let only the protected offline software signer or hardware signer sign matching exact manifest bytes. Publish the same raw signature as the detached signature with the unchanged archive at its immutable manifest-digest address. |
| `feat(runtime): verify accepted PostgreSQL trees` | `crates/orna-postgres-runtime/Cargo.toml`; `crates/orna-postgres-runtime/src/lib.rs`; `Cargo.lock` | Embed and strictly validate the accepted record, then expose only signature-, ABI-, ancestor-, and payload-verified absolute program handles. |
| `test(runtime): reject untrusted PostgreSQL trees` | `crates/orna-postgres-runtime/tests/runtime_tree.rs` | Prove the strict five-key accepted-record schema and validation order, first-runtime-identity rejection before digest or key checks, public-key and release-key derivation, manifest-byte signature verification, inventory, ELF closure, metadata, link, ancestor, and hostile-environment rejection. |
| `build(server): declare host dependencies` | `crates/orna-server/Cargo.toml`; `Cargo.lock` | Feature-pin the runtime verifier, Tokio and tokio-postgres, SHA-256 and TOML, direct account-file, path, `openat2`, `fcntl`, signal, and systemd notify and service-state dependencies needed by every later production row, without changing behaviour. |
| `feat(server): model the default instance host` | `crates/orna-server/src/runtime.rs`; `crates/orna-server/src/lib.rs` | Add service-account, trusted-path, filesystem, lifetime-lock, durable-manifest, generation, transition, and ready-record types. |
| `test(server): prove instance host invariants` | `crates/orna-server/tests/instance_host.rs` | Prove owners, modes, links, EUID, filesystems, instance locking, manifest durability, and stale-readiness behaviour. |
| `feat(server): own the package protocol` | `crates/orna-server/src/package_maintenance.rs`; `crates/orna-server/src/main.rs`; `crates/orna-server/tests/package_maintenance.rs` | Implement the private `begin` and `complete` helper dispatch, shared public-command lifetime locks, exact state bytes and diagnostic, atomic persistence, and first-install creation. |
| `feat(server): initialise private PostgreSQL` | `crates/orna-server/src/runtime.rs`; `crates/orna-server/tests/initialise_runtime.rs` | Run exact private initdb, bootstrap authentication, database creation, final peer authentication, and checksum proof. |
| `feat(server): supervise private PostgreSQL` | `crates/orna-server/src/runtime.rs`; `crates/orna-server/src/lib.rs`; `crates/orna-server/src/main.rs` | Implement no-argument server run under its retained package read lock, recovery gates, pre-ready activation persistence, systemd readiness, direct-child supervision, and bounded stop. |
| `build(debian): define the managed service` | `packaging/debian/orna.sysusers`; `packaging/debian/orna.tmpfiles`; `packaging/debian/orna.service` | Create the locked account and exact normal and maintenance runtime directories, then apply notify, mixed-kill, timeout, and rate-limited restart rules. |
| `test(server): prove systemd supervision` | `crates/orna-server/tests/bundled_runtime.rs` | Prove direct-child exits, leaked descendants, cgroup cleanup, restart gates, and no in-process restart. |
| `feat(server): add offline PostgreSQL upgrade` | `crates/orna-server/src/runtime.rs`; `crates/orna-server/src/main.rs` | Implement no-argument same-major and copy-based major maintenance under retained package and instance locks with durable interruption re-entry. |
| `test(server): prove upgrade transitions` | `crates/orna-server/tests/runtime_upgrade.rs`; `crates/orna-server/tests/fixtures/fake-pg-tool`; `crates/orna-server/tests/fixtures/test-runtime-key` | Prove the generic state machine and interruption boundaries with fake tools and a test signing key. |
| `feat(server): bind backend-shell to the ready host` | `crates/orna-server/src/backend_shell.rs`; `crates/orna-server/src/main.rs`; `crates/orna-server/tests/backend_shell.rs` | Remove URL and inherited-`PATH` authority, carry the package read lock through `exec`, supply the fixed private path, require the service identity and ready host, add `--no-password`, and preserve exact diagnostic precedence. |
| `build(debian): ingest signed runtimes` | `packaging/debian/ingest-runtime.sh`; `packaging/debian/tests/signed-runtime.sh` | Fetch only the immutable protected archive by accepted digest and reject an archive, signature, manifest, payload, or candidate mismatch. |
| `build(debian): define the Orna package` | `packaging/debian/control`; `packaging/debian/rules`; `packaging/debian/changelog` | Build one Debian 12 amd64 package, bound `libc6:amd64` and `libc-bin` to `>= 2.36` and `< 2.37`, declare `dash` and `libgcc-s1`, verify the public Orna ELF closure and helper byte identity, invoke verified runtime ingestion, and require every earlier accepted private root. |
| `build(debian): install the private layout` | `packaging/debian/orna.install`; `packaging/debian/default.toml`; `packaging/debian/copyright` | Install one public binary, its private same-byte package helper, every retained runtime and evidence file, the inspectable runtime-free default configuration, and required licence notices. |
| `build(debian): drive the package protocol` | `packaging/debian/preinst`; `packaging/debian/prerm`; `packaging/debian/postinst` | Encode exact old-prerm/new-preinst ordering, use only absolute `/usr/bin/dpkg --compare-versions` for version ordering, reject a downgrade before `begin`, call only the existing or newly unpacked absolute helper for package-protocol transitions, permit the two helper-less install forms, perform no data transition, and leave the service stopped. |
| `test(debian): prove package transaction exclusion` | `packaging/debian/tests/package-install.sh` | Prove first install, equal-version and forward update, the sole absolute dpkg comparison path, exact downgrade diagnostic and exit with unchanged ready state, repair, removal, purge, reinstall after removal and purge, shared-reader conflicts, commit-boundary missing/incomplete/ready states, byte identity, append-only roots, and fault-injected fail-closed re-entry. |
| `test(debian): prove a self-contained install` | `packaging/debian/clean-machine-test.sh`; `.github/workflows/debian-package.yml` | Prove the public Orna ELF closure, signed private binary-tree match, absolute version probes, bootstrap, standard persistence, restart recovery, shell, hostile host, removal, and no network or external backend. |

The first development tracer ends after the deterministic build and absolute
version probes. The first production tracer requires every row through the
clean Debian package gate. Standard-library work continues as a dependency of
the start and recovery gate, not as a replacement for this product-level
distribution work.

The first production release contains only PostgreSQL 18.4. Generic same-major
and major transition tests use the test key and fake tool fixtures. A real
same-major or major live upgrade gate becomes mandatory only for a release that
adds a second accepted runtime. The first production tracer does not claim a
live transition that cannot yet exist.

## Deferred surface

This record does not accept:

* a production dependency on an installed PostgreSQL command, package,
  service, Docker image, container, remote database, or first-start download;
* a public PostgreSQL executable, pgwire endpoint, SQL interface, PostgreSQL
  driver contract, or PostgreSQL compatibility promise;
* an in-process PostgreSQL fork, a `libpq` server, or a claim that PostgreSQL
  can run without its process and support-file tree;
* production use of the external development adapter or fallback from failed
  private verification or startup to an ambient backend;
* another operating system, Debian version, glibc ABI, CPU architecture,
  package format, durable filesystem, or a single self-extracting executable;
* multiple named instances, user-selected runtime roots, PostgreSQL
  extensions, preload libraries, tablespaces, or arbitrary PostgreSQL
  configuration;
* a private TCP connection, private TLS, password authentication, remote raw
  administration, or public lifecycle operation;
* high availability, replication, point-in-time recovery, backup and restore,
  online major upgrade, or automatic repair of a damaged cluster;
* direct `pg_resetwal` repair outside verified `pg_upgrade`, automatic
  reinitialisation, package-script data migration, deletion of durable state,
  reverse same-major startup after the candidate-may-open phase, or major
  rollback after activation commit; or
* the future Orna request-service transport and authentication, package
  repository policy, release-key rotation ceremony, and cleanup policy for old
  runtime roots or data generations.

Each deferred production surface requires its own accepted security,
compatibility, lifecycle, or recovery rule.

## Consequences

An operator installs Orna, not PostgreSQL. Orna owns the exact backend source,
build, dependency closure, private process, connection, configuration,
initialisation, startup, shutdown, recovery, upgrade, and raw shell target.
PostgreSQL remains replaceable behind Orna's private kernel interface and does
not become a public language or protocol.

The package is larger. Each release must build, reproduce, review, sign,
licence, scan, retain, package, and test PostgreSQL and its private dependency
closure. Orna must also maintain systemd supervision and crash-safe generation
transitions. These costs are part of delivering Orna without transferring
backend installation and version selection to the operator.

The private runtime is byte-reproducible and signed, while Debian package trust
is authoritative for supported glibc, libc-bin, dash, and libgcc security
updates within the Debian 12 target and declared libc bounds. Orna's runtime
signature is authoritative only for immutable Orna-owned PostgreSQL bytes.
Runtime path and ELF checks still fail closed at the operating-system boundary.

The external development backend remains useful for fast integration tests.
It is not evidence that a production installation is self-contained. The
signed clean-machine package gate provides that evidence.

## Precedence

This record implements work ADR 0001's private PostgreSQL kernel and preserves
its no-public-pgwire rule. It preserves work ADR 0004's protected schemas,
stable physical names, transactional apply, and fail-closed durable recovery.
It does not change source authority, catalogue identity, standard-library
authority, or PostgreSQL's status as a private storage encoding.

For work ADR 0014, this record supersedes:

* attachment to an installed `psql`;
* the generic caller identity in favour of the exact `orna` effective
  service-account identity;
* `ORNA_SERVER_POSTGRES_URL`, TCP server-host configuration, and all URL
  diagnostics;
* `PATH` lookup and inherited or reconstructed libpq connection variables;
* the installed-program argument and environment contract;
* the backend-shell-only one-line usage copy in favour of the global
  three-command usage text in this record;
* attachment to an available backend without an already-ready managed host;
  and
* deferral of a Unix socket, packaging, `psql` version, PostgreSQL lifecycle,
  server-host process, and service manager.

This record preserves work ADR 0014's exact backend-shell command shape,
all-terminal requirement, no-elevation rule, local operator boundary,
attach-only behaviour, raw-administration boundary, absence of source,
catalogue, function, artefact, or audit evidence for shell use, and native
`psql` behaviour after process replacement. Backend-shell performs no
pre-attachment durable write. Docker Compose remains only the explicit
development test adapter accepted here.

This record resolves the first PostgreSQL installation, host ABI, package,
process lifecycle, and upgrade parts left open by
`spec/docs/36-storage-transactions.md`,
`spec/docs/38-implementation-roadmap.md`, and
`spec/docs/41-open-questions.md`. It does not alter standard-library module
distribution questions in `spec/docs/37-modules-distribution.md` or public
runtime-install questions in `spec/docs/15-runtime-architecture.md`.

For production PostgreSQL distribution, instance ownership, private local
connection, process lifecycle, backend-shell target, and backend update rules,
this accepted record has precedence.

## References

* [PostgreSQL 18.4 source archive checksum](https://ftp.postgresql.org/pub/source/v18.4/postgresql-18.4.tar.bz2.sha256)
* [PostgreSQL 18 `initdb`](https://www.postgresql.org/docs/18/app-initdb.html)
* [PostgreSQL 18 `pg_upgrade`](https://www.postgresql.org/docs/18/pgupgrade.html)
* [PostgreSQL 18 libpq client interface](https://www.postgresql.org/docs/18/libpq.html)
* [PostgreSQL 18 peer authentication](https://www.postgresql.org/docs/18/auth-peer.html)
* [PostgreSQL 18 user-name maps](https://www.postgresql.org/docs/18/auth-username-maps.html)
* [PostgreSQL 18 server shutdown signals](https://www.postgresql.org/docs/18/server-shutdown.html)
* [Linux `openat2(2)`](https://man7.org/linux/man-pages/man2/openat2.2.html)
* [Debian Policy: maintainer scripts](https://www.debian.org/doc/debian-policy/ch-maintainerscripts.html)
* [Debian 12 `dpkg(1)` `--compare-versions`](https://manpages.debian.org/bookworm/dpkg/dpkg.1.en.html)
* [Debian 12 `nsswitch.conf(5)`](https://manpages.debian.org/bookworm/manpages/nsswitch.conf.5.en.html)
* [glibc built-in `nss_files`](https://sourceware.org/pipermail/glibc-cvs/2021q3/073626.html)
* [Debian 12 `systemd.kill(5)`](https://manpages.debian.org/bookworm/systemd/systemd.kill.5.en.html)
