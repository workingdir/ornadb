# Work ADR 0100: OrnaDB 1.0 CLIENT VM Trust Boundary

**Status:** Accepted 1.0 boundary; production CLIENT VM trust and sandbox are deferred

## Decision

For remote Orna source evaluation, the host is authoritative: it parses,
resolves, type-checks, and executes submitted source through the same language
implementation and activation semantics as the local REPL. A client-supplied
AST, bytecode, or query plan is not authoritative (**ORNA-EVAL-002**,
`source/14-pages.md`, “Explicit remote Orna evaluation”). Evaluation is an
explicit operation, not implicit interpretation of ordinary values
(**ORNA-EVAL-001**, same section). Watches retain the read-only effect boundary
in **ORNA-EVAL-004**.

The trusted v1 deployment boundary remains the invoking OS user and an
explicitly trusted network perimeter (**ORNA-TRUST-001** through
**ORNA-TRUST-003**, `source/26-security.md`, “Trust, isolation and extensions”).
The build metadata requirement is limited to identifying the complete source
snapshot and compatibility coordinates used to produce an artifact
(**ORNA-SYS-072**, `source/15-system.md`, § “History, plans and storage”). It
does not define artifact signatures, signer identity, provenance policy, or
verification outcomes.

The portable-extension rules remain extension-scoped: WIT imports/exports and
the pinned Git object identify extension code (**ORNA-EXT-002**); Wasm receives
typed host calls rather than raw Turso/Git handles (**ORNA-EXT-003**); coarse
budgets and cancellation are recommended for portable components
(**ORNA-EXT-004**); native extensions use host-process trust and are not
sandboxed by Orna (**ORNA-EXT-005**), all in `source/26-security.md`,
“Extensions”. These clauses do not define CLIENT VM artifact admission or
CLIENT sandbox semantics.

Accordingly, OrnaDB 1.0 accepts no production CLIENT bytecode VM contract,
artifact distribution/loading protocol, signature or provenance trust policy,
CLIENT capability broker, or CLIENT sandbox guarantee. These features are
deferred until a normative contract defines their authorities, inputs, failure
behavior, and host boundary. Work ADR 0091 remains a proposal/deferment; its
proposed mechanisms are not accepted 1.0 behavior. This decision does not
weaken the host-side source evaluation requirements above.

## Search evidence

The following bounded search was run against the frozen reference's normative
source and supporting grammar, tests, examples, and API directories:

```sh
rg -n -i 'CLIENT (VM|bytecode|artifact)|client.*(sandbox|capability)|artifact.*(signature|provenance|verif|attest)|ambient.authority|sandbox broker' \
  /home/pbox/dev/ornadb/reference/Orna-1.0.0/{source,grammar,tests,examples,api}
```

It returned no matching lines (exit 1). The reference does contain relevant,
narrower evidence: `tests/scenarios.json` scenario `EVAL-001` maps explicit
remote source evaluation to **ORNA-EVAL-001** through **ORNA-EVAL-003** and is
labelled an implementation scenario, not an executed engine test; scenario
`SEC-001` maps the trusted local server boundary to **ORNA-SERVE-004**,
**ORNA-SERVE-006**, **ORNA-TRUST-001**, and **ORNA-TRUST-002**. The
`sys.ClientKind` values `sandboxed` and `trusted` in
`source/34-system-reference.md` describe a client-kind value; they do not
establish a CLIENT VM artifact security contract.

## Precedence

The frozen OrnaDB 1.0.0 reference is authoritative. This ADR records the scope
it accepts and defers; it does not add language syntax, runtime behavior,
artifact formats, or security guarantees absent from that reference.
