# ADR 0098: Ring-1 Catalogue and Revision Gateway Deferral

**Status:** Deferred pending an accepted sealed-function contract

## Decision

Keep the Orna 1.0.0 system API behavior normative, but defer adding catalogue,
source, dependency, and revision operations to the sealed Ring-1 `sys.invoke`
registry until an authoritative contract identifies the accepted Ring-1
entries and their sealed identities.

Do not assign `FunctionId`s, result-carrier identities, or runtime authority
for these entries by inference. Existing reference-defined names, signatures,
and semantics remain the target conformance contract; this decision does not
replace them with alternate behavior or claim that their full implementation
is complete. The deferred scope is their availability through sealed Ring-1
invocation.

## Context

The frozen Orna 1.0.0 reference defines catalogue metadata and the behavior of
system source, history, and dependency operations. It does not identify which
of those operations are mandatory pre-catalogue Ring-1 functions, assign their
sealed function identities, or define the corresponding sealed runtime
authority. The existing task `ornadb-1787784781797-53-6e72faaf` is conditional
on acceptance of that contract and explicitly prohibits exposing proposal APIs
before the decision.

The project registry in `orna-core` is a sealed list keyed by stable
`FunctionId`s. Registering a new entry therefore requires choosing identity
and authority outside the behaviors specified by the reference. Until those
are accepted, the implementation must not guess them.

## Reference evidence

The frozen reference at `reference/Orna-1.0.0` is the sole normative source
for the following behavior:

* `Orna-1.0.0.md` and `source/15-system.md` define snapshot-qualified names,
  source and semantic hashes, generated provenance, and dependency traversal.
  Relevant requirements are `ORNA-SYS-033`, `034`, `035`, `036`, `043`,
  `044`, and `045`.
* `source/15-system.md` defines reflective invocation constraints:
  `ORNA-SYS-056` requires an invocation identity before target code begins;
  `ORNA-SYS-077` forbids bypassing parsing, resolution, type/effect checks, or
  transaction rules; `ORNA-SYS-129` through `132` constrain system types,
  exact typed values, argument maps, and result witnesses.
* `source/34-system-reference.md` and `api/sys.json` enumerate the signatures
  and relation descriptors for `sys.source`, `sys.history`, `sys.dependencies`,
  and `sys.dependents`, plus the catalogue relations. They specify API shape
  and behavior but do not give a Ring-1 registry list or sealed `FunctionId`s
  for these entries.
* `grammar/orna.ebnf` defines `generic_call_expression` as a qualified name
  with explicit type arguments and arguments. It does not classify a qualified
  name as a sealed Ring-1 entry.
* `tests/requirements.json` states `ORNA-SYS-128`: the suite must exercise both
  `sys.source` and `sys.history` overloads across current, renamed, historical,
  missing, partially cloned, and redacted cases. The matching entry in
  `tests/requirement-evidence.json` is marked `not executed` with status
  `planned`; it is a conformance obligation, not a registry-acceptance decision.
* `examples/valid/sys-definition-file.orna`,
  `examples/valid/sys-file-history.orna`, and
  `examples/valid/sys-table-query.orna` demonstrate reference source syntax.
  They do not assign sealed function identities or runtime authority.

## Reopening condition

Reopen implementation when an authoritative Orna 1.0.0 contract identifies
the accepted Ring-1 catalogue/source/dependency/revision entries and their
sealed identities and authority. Then implement only those accepted entries,
with Rust tests loading real `.orna` fixtures via `include_str!` and coverage
for the applicable `ORNA-SYS-*` requirements.

This deferral records a missing implementation boundary; it does not amend
the frozen reference or weaken any normative requirement.
