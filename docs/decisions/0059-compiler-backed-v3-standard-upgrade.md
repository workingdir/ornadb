# ADR 0059: Compiler-Backed `orna.std/3` Standard Upgrade

**Status:** Accepted

## Decision

The append-only standard upgrade from `orna.std/2` to `orna.std/3` is
implemented through the same compiler-backed install pipeline that work ADR
0055 established for the V1-to-V2 transition. `orna.std/3` is installed into
a database by a normal `prepare_standard_upgrade_v2_to_v3` +
`apply_standard_upgrade` sequence, not by a test-hooks fixture.

This decision supersedes the fail-closed stub in ADR 0058
(`UnsupportedStandardUpgrade`). It does not modify `orna.std/1`, `orna.std/2`,
or the V3 retained source bytes, identities, or digest goldens.

## Snapshot facts (unchanged from ADR 0058)

| Fact | Value |
| --- | --- |
| Standard version | `orna.std/3` |
| Standard revision / catalogue / bundle / source revision | `...03` |
| Source-revision parent | V2 source revision `...02` |
| Units | `std/types.orna` `...02`, `std/invoke.orna` `...03`, `std/output.orna` `...04` |
| New schemas | `std.terminal` `...04`, `std.io` `...05` |
| New value types | `std.terminal.Document` `...15`, `std.io.ByteStream` `...16` |
| Digest contract | V2 domain (`ornadb.hash/standard-library/v2\0`), version number 2 |

The V3 snapshot reuses the V2 type catalogue, the one `std.invoke.echo`
executable, its artifact, and its semantic digest unchanged. The three-unit
bundle order is part of the V3 digest.

## Compiler reconciliation

`check_standard_library_source` gains a V3 branch that:

1. requires exactly three ordered units with the exact identities and
   logical paths (`std/types.orna` `...02`, `std/invoke.orna` `...03`,
   `std/output.orna` `...04`);
2. reconciles the types unit exactly as the V2 checker does (schemas, value
   types, bindings against the snapshot catalogue);
3. reconciles the invoke unit exactly as the V2 checker does (`std.invoke`
   schema + `std.invoke.echo` via `check_standard_parameter_echo`);
4. reconciles the output unit: exactly `std.terminal` and `std.io` schema
   declarations and exactly the two opaque value types
   (`std.terminal.Document` `...15`,
   `std.io.ByteStream` `...16`) with their `KERNEL CONTRACT`s and `EXPORT`
   bindings, matching the snapshot catalogue and origins byte-exactly;
5. cross-checks the snapshot `StandardExecutable` exactly as the V2 checker
   does (the V3 executable is the unchanged `std.invoke.echo`).

Every stored declaration, identity, resolved signature, artifact,
reference, and origin must agree with the retained units, or the check fails
closed. The V1 and V2 checker paths remain byte-identical.

## Upgrade pipeline

`prepare_standard_upgrade_v2_to_v3(active)`:

1. fails closed when the active revision pins any standard other than
   `orna.std/2` (already installed, or wrong base);
2. retains and verifies the immutable `orna.std/2` snapshot (the parent
   must be present; V3 is the append-only child);
3. retains and verifies `orna.std/3` via
   `retained_standard_library_v3_snapshot` /
   `verify_standard_library_v3_snapshot`;
4. checks the V3 snapshot with `check_standard_library_source`;
5. prepares the companion application revision through the existing
   `prepare_checked_standard_upgrade` machinery (fresh application source
   bundle/revision/unit identities, catalogue revision, and the V3-verified
   hash context), exactly as the V1-to-V2 path does.

The PostgreSQL apply path (`apply_standard_upgrade`) persists the complete
V3 snapshot (header, three units with ordinals, catalogue, executable,
origins, references) and the companion application revision atomically,
using the migration-0023 standard relations. Historical application
revisions keep their V1/V2 pins; the new active revision pins `orna.std/3`.

The kernel reserved-identity scan and recovery reconstruction already
handle the V2 digest domain; the V3 snapshot reuses it, so the V2
persistence/recovery code paths accept V3 without new relations. Recovery
reconstructs the three-unit bundle, verifies the V3 standard digest, and
issues the verified-standard capability exactly as for V2.

## Required implementation order

1. `docs(std): define the compiler-backed v3 upgrade` — this ADR and the
   work-ADR index only.
2. `feat(compiler): reconcile the v3 output source unit` — the V3 branch of
   `check_standard_library_source`, the output-unit reconciliation, and
   focused tests; V1/V2 paths byte-identical.
3. `feat(std): install the v3 snapshot through the upgrade pipeline` —
   `prepare_standard_upgrade_v2_to_v3` calls the shared prepare machinery
   (removing the `UnsupportedStandardUpgrade` stub), with fail-closed tests
   for wrong bases and already-installed states.
4. `test(postgres): prove the v3 install and reopen` — a live proof that a
   fresh database installs V1, upgrades to V2, upgrades to V3 through the
   normal `prepare_standard_upgrade_v2_to_v3` + `apply_standard_upgrade`
   path, reopens with the V3 pin, rejects tampered V3 rows, and keeps
   historical pins intact.

Each commit changes one to three files, has a signed Conventional Commit, and
keeps the workspace buildable.

## Deferred surface

`orna.std/4` and later snapshots, a V3-specific digest domain, presenter
functions as standard executable objects (std.json.encode and
std.terminal.present_table become real registered standard functions in a
later ADR), CSV/XML encoders, and graphical runtimes remain future
decisions.

## Precedence

Work ADR 0055 remains authoritative for `orna.std/2` immutability, the
upgrade authority rule, and the V2 digest contract. Work ADR 0058 remains
authoritative for the V3 snapshot facts and codec registration. This
decision adds only the compiler-backed install path and supersedes the
`UnsupportedStandardUpgrade` stub within that scope. The canonical
specification remains authoritative outside this accepted implementation
scope.
