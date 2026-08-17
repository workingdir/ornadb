# ADR 0069: CLIENT STATE Declarations and Function-Instance State

**Status:** Accepted

## Decision

CLIENT functions gain the first executable state-declaration form from the
canonical function grammar:

```sql
CREATE CLIENT FUNCTION studio.connections()
RETURNS TEXT
IS
    STATE filter TEXT SCOPE LOCAL DEFAULT '';
    STATE selected TEXT SCOPE SESSION DEFAULT NULL;
BEGIN
    RETURN filter;
END;
```

The implementation starts with a closed block subset. It records state slots
in the checked CLIENT function and in a version-4 client plan. The client
runtime owns `LOCAL` and `SESSION` values. `USER` declarations are carried as
state metadata and use the existing `sys.state.*` service in a later runtime
slice; they do not become unprotected database writes in this ADR.

The declaration surface follows `spec/spec/orna.ebnf`:

```text
STATE identifier type_spec
    [ SCOPE (LOCAL | SESSION | USER) ]
    [ DEFAULT expression ] ;
```

An omitted scope means `LOCAL`. An omitted default means an unset value. The
state type must be a supported scalar or registered opaque value type, and a
default expression must have the declared type when it is present.

## Closed block subset

This ADR accepts the following body shape:

```text
IS
    { state_declaration }
BEGIN
    RETURN [ expression ] ;
END
```

The return expression uses the closed expression vocabulary from work ADR
0068. The body has exactly one return statement. `LET`, `CONST`, assignments,
ordinary call statements, `AWAIT`, conditionals, loops, SQL statements, and
other procedural statements remain rejected until a later ADR adds their
checked semantics. This restriction keeps the first state slice deterministic
and prevents an unimplemented procedural language from being accepted as if it
were executable.

The parser keeps declaration order and source spans. The compiler resolves
state types, validates defaults, rejects duplicate names, and adds each slot
to the checked function model. State declarations do not change the meaning
of existing expression-body or Boolean CLIENT functions.

## State-slot identity

A state slot is identified by its owning function identity and a checked slot
identity. The compiler allocates a provisional `StateSlotId` while checking a
new function and emits the durable identity in the prepared client plan.

This ADR does not invent source-level rename syntax. An ordinary source name
change is therefore not treated as an identity-preserving rename. The
identity-preserving rename and delete-and-recreate rules in
`spec/docs/16-state-model.md` require a later semantic-diff or explicit rename
surface before durable USER state can rely on them. The implementation must
not silently claim that a source rename preserves persisted state.

## Plan format

Version 4 extends the client-plan family with state-slot metadata and retains
the version-3 expression operation for functions with no state declarations.
The version-4 operation contains:

1. the checked return expression;
2. the ordered state-slot records;
3. each slot's `StateSlotId`, `TypeId`, scope, and optional default plan.

The encoding contains identities and typed plan data only. It contains no
source text, source locations, Orna names, or backend values. The decoder
bounds the number of slots and reuses the existing expression and opaque-value
limits.

A function without state declarations continues to use its existing plan
format and revision path. Adding state creates a new function revision. A
state declaration does not change the catalogue identity of its owning
function.

## Runtime semantics

`LOCAL` state lives in the mounted function instance and is never sent to the
server unless a later explicit call uses its value. `SESSION` state lives for
the client invocation session and may survive a compatible remount when the
runtime supplies the same instance identity. `USER` state is associated with
the authenticated principal by the server; the client never supplies a
principal identity.

This ADR provides the metadata boundary only. The following runtime work is
separate and must preserve the state model in spec ADR 0007 and work ADR 0061:

- function-instance keys and root state profiles;
- initial USER-state batch loading;
- debounced and coalesced USER-state writes;
- revision conflict handling and reconciliation;
- administrative cross-principal inspection;
- state references from procedural expressions.

Until those slices land, a runtime must fail closed rather than silently drop
or persist a declared state value.

## Alternatives considered

### Encode state in the existing version-3 expression tree

This keeps one plan version, but it mixes declaration metadata with a return
expression format that deliberately contains no state model. It would also
make the v3 decoder accept new semantics without a version boundary. Rejected
because a version-4 boundary makes the contract explicit and keeps old plans
byte-compatible.

### Add a separate state catalogue relation before parsing CLIENT declarations

A relation could provide durable slot identities and rename history, but it
would require catalogue migration and source-apply rules before the parser can
accept the first state declaration. Rejected for this first vertical slice;
that work belongs with the later semantic rename and USER persistence design.

### Accept the complete procedural grammar now

This would accept syntax for assignments, control flow, SQL, and `AWAIT`
without checked semantics or safe runtime execution. Rejected because it
would create misleading accepted source and hidden runtime behaviour.

## Required implementation order

1. `docs(client): define CLIENT STATE declarations` - this ADR and the work
   ADR index.
2. `feat(syntax): parse CLIENT state blocks` - parse state declarations and
   the single-return block without changing existing body behaviour.
3. `feat(compiler): check CLIENT state slots` - add checked scope, type, default,
   and provisional slot identity facts.
4. `feat(artifact): encode version-four CLIENT state plans` - bounded canonical
   metadata and round-trip tests.
5. `feat(client): initialise LOCAL and SESSION state` - expose explicit state
   input/output in the evaluator and fail closed for unsupported USER use.
6. `feat(client): connect USER state lifecycle` - add authenticated batch load,
   coalesced writes, instance identity, and conflict handling through the
   sealed service.

Each commit changes one to three files, uses a Conventional Commit subject,
and leaves the workspace buildable.

## Deferred surface

Full procedural statements, state reads and writes in expressions, function
instance identity, root state profiles, USER-state lifecycle, async calls,
resources, streams, `AWAIT`, and graphical runtime state remain later ADRs.

## Precedence

This decision extends work ADR 0068 and implements the CLIENT state grammar
surface in the canonical specification. Work ADR 0061 remains authoritative
for durable USER state and principal derivation. Spec ADR 0007 and
`spec/docs/16-state-model.md` remain authoritative outside this accepted
implementation scope.
