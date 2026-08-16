# ADR 0060: CLIENT Capability Requirements and the Local Sandbox

**Status:** Accepted

## Decision

CLIENT functions may declare capability requirements, and the local `orna`
client enforces them in a closed sandbox gate before evaluation. This is the
first CLIENT VM foundation slice (spec milestone 7): it makes
`REQUIRES CAPABILITY` a real, checked, enforced contract for CLIENT
functions, and it establishes the local capability grant model that later
slices (resources, actions, USER state, std.ui) will build on.

The grammar already parses `CapabilitySpecification` for SERVER functions.
This decision:

1. accepts the same capability clause on `CREATE CLIENT FUNCTION`
   declarations (the syntax type `ClientFunctionDeclaration` gains a
   `capabilities` field; the parser accepts the clause after `RETURNS`);
2. defines the closed capability vocabulary for this slice:
   `std.fs.read(path-scope)`, `std.fs.write(path-scope)`,
   `std.net.connect(host-scope)`, `std.secret.use(secret-id)` —
   each a qualified name plus a bounded argument list;
3. checks the clauses at compile time: names must be in the closed
   vocabulary, argument counts and shapes must match the vocabulary entry,
   and a CLIENT function must declare exactly the capabilities its body
   uses (over-declaration and under-declaration both fail closed);
4. enforces at invocation time: the local client holds a
   `LocalCapabilityGrant` set (from local configuration, not from the
   database), and `sys.invoke`/the CLIENT dispatch path refuses to evaluate
   a CLIENT function whose required capabilities are not all granted;
5. records the capability decision in the protected security audit (a new
   closed `SecurityAuditKind::Capability` decision), so capability denials
   are inspectable without disclosing path/secret details.

The sandbox itself (filesystem/network access mediation) is a later slice;
this decision is the declarative gate that admits or rejects evaluation
before any sandboxed operation runs. A CLIENT function with zero declared
capabilities evaluates exactly as today.

## Syntax

`CREATE CLIENT FUNCTION` gains the optional clause after `RETURNS`:

```sql
CREATE CLIENT FUNCTION local.hash_file (
    p_file std.fs.Path
)
RETURNS BYTES
REQUIRES CAPABILITY std.fs.read(p_file)
IS
BEGIN
    RETURN std.crypto.sha256(std.fs.read_all(p_file));
END;
```

The `REQUIRES CAPABILITY` clause appears once, after the return type and
before the body (`AS`/`IS`). The grammar reuses the existing
`CapabilitySpecification` form: qualified name, optional parenthesised
argument list, exact source span. A second clause, a clause after the body,
or a clause on a SERVER function body in this slice fails closed (SERVER
capability requirements remain rejected exactly as today).

## Capability vocabulary

Each capability has a fixed name, a fixed argument shape, and a closed
interpretation:

| Capability | Argument shape | Interpretation |
| --- | --- | --- |
| `std.fs.read` | one path-scope argument (text literal or parameter reference) | read access to the resolved path scope |
| `std.fs.write` | one path-scope argument | write access to the resolved path scope |
| `std.net.connect` | one host-scope argument | connect to the resolved host scope |
| `std.secret.use` | one secret-id argument | use the named secret |

The vocabulary is compiled into the compiler (a closed table). An unknown
capability name, a wrong argument count, or an argument that is neither a
literal nor a declared parameter fails closed at compile time. The
vocabulary is deliberately small; later ADRs extend it (std.clipboard,
std.process, std.net.listen).

For this slice the compiler records the requirement fact but does not
analyse the body for actual capability use beyond the closed Boolean-literal
body forms: a CLIENT function whose body is the accepted Boolean literal or
opaque-value form may declare zero capabilities only; a capability
declaration on such a function fails closed ("the accepted CLIENT body
performs no capability-requiring operation"). Later body forms will drive
the over/under-declaration check against real operations.

## Compile-time check

`check_client_functions` (orna-compiler/src/resolver.rs:3443) gains:

- vocabulary validation: each declared capability name and argument shape
  must match the closed table;
- body compatibility: the accepted CLIENT body forms (Boolean literal,
  opaque value) must declare zero capabilities;
- reference validation: every capability argument that names a parameter
  must name a declared parameter of the function.

Every failure is a typed diagnostic with the declaration span.

## Invocation-time enforcement

The sealed `sys.invoke` CLIENT path and the raw CLIENT dispatch path share
one gate:

1. the target is a CLIENT function;
2. the local client supplies its `LocalCapabilityGrant` set (derived from
   local configuration in `orna-client`, never from the database);
3. the gate requires every declared capability of the function revision to
   be present in the grant set, with the declared argument-scope
   compatibility checked against the grant's scope where the vocabulary
   defines scope (a grant `std.fs.read(/home/bob)` satisfies
   `std.fs.read(p_file)` whose resolved value is under that scope);
4. missing grants fail closed with a closed capability-denial (redacted:
   no path/secret details) and a `SecurityAuditKind::Capability` denied
   decision;
5. all grants present: evaluation proceeds exactly as today.

The grant set is a checked value in `orna-client` (`LocalCapabilityGrant`),
constructed from local configuration with closed validation. The server
kernel treats capability enforcement as client-side authority: the sealed
boundary may reject a CLIENT target whose declared capabilities cannot be
honoured by the offer, but the actual grant decision is the client's.

## Audit

`SecurityAuditKind` gains `Capability`. The denied decision records the
function identity and the redacted capability name (qualified name only, no
arguments). The allowed decision records the same. No path, host, or secret
argument value is ever written to audit.

## Required implementation order

1. `docs(client): define CLIENT capability requirements` — this ADR and the
   work-ADR index only.
2. `feat(syntax): parse CLIENT capability clauses` — the
   `ClientFunctionDeclaration.capabilities` field, the parser clause after
   `RETURNS`, and parse tests.
3. `feat(compiler): check CLIENT capability requirements` — the closed
   vocabulary table, compile-time validation, and rejection tests.
4. `feat(client): model local capability grants` — `LocalCapabilityGrant`
   and the closed grant-set construction/validation in orna-client, with
   tests.
5. `feat(security): audit capability decisions` — the `SecurityAuditKind::
   Capability` decision and its encode/decode/recovery in orna-core +
   orna-postgres, with tests.
6. `feat(server): enforce the capability gate` — the shared gate in the
   sealed and raw CLIENT paths, redacted denials, and unit tests.
7. `test(server): prove the capability gate end to end` — a live proof
   that a CLIENT function with a granted capability evaluates, with a
   missing grant denies closed and audits the decision.

Each commit changes one to three files, has a signed Conventional Commit, and
keeps the workspace buildable.

## Deferred surface

The sandbox itself (path/host/secret mediation), resource/action types,
LOCAL/SESSION state, function-instance identity, trace hooks, USER state,
std.ui, and the rest of the CLIENT VM are later ADRs. SERVER capability
requirements remain rejected. The capability vocabulary is closed to the four
entries above until a later ADR extends it.

## Precedence

This decision implements the capability-checking part of spec milestone 7
and spec ADR 0022's authorisation gate for CLIENT evaluation. Work ADR 0020
remains authoritative for EXECUTE authorisation; this decision adds a
client-side capability requirement on top of it. The canonical specification
remains authoritative outside this accepted implementation scope.
