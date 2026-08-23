# ADR 0077: CLIENT-to-SERVER Resource Language Surface

**Status:** Accepted

## Decision

Implement the source surface accepted by spec ADR 0017. CLIENT-to-SERVER work
uses standard-library resource and action values. The language does not add a
core `RESOURCE` or `ACTION` statement.

The canonical constructor forms are:

```sql
std.data.resource(
    target => tasks.overdue,
    arguments => std.call.args(p_owner => owner)
)
```

and:

```sql
std.data.stream_resource(
    target => studio.execute_sql,
    arguments => std.call.args(p_source => source)
)
```

The first returns `std.data.Resource<T>`. The second returns
`std.data.StreamResource<T>` for a target whose declared return type is
`STREAM<T>`. The constructor is a typed, non-blocking value operation. It
selects the active target revision and creates or reuses the complete local
resource identity defined by work ADR 0071.

`AWAIT resource` is a CLIENT-only suspension expression. It is valid in a
procedural CLIENT body at a `LET`, assignment, or `RETURN` expression. The
runtime yields while the resource is `IDLE` or `LOADING`, resumes with the
checked value in `READY`, and returns a structured failure for `FAILED` or
`CANCELLED`. `AWAIT` on a scalar resource returns `T`. `AWAIT` on a stream
resource returns `OPTION<LIST<T>>`, with `None` after a successful terminal
completion.

Canonical source:

```sql
CREATE CLIENT FUNCTION studio.overdue_rows(p_owner REF studio.owner)
RETURNS TABLE(task_id UUID, title TEXT)
IS
    LET rows std.data.Resource<TABLE(task_id UUID, title TEXT)> :=
        std.data.resource(
            target => tasks.overdue,
            arguments => std.call.args(p_owner => p_owner)
        );
BEGIN
    RETURN AWAIT rows;
END;
```

The compiler accepts only one named `target` and one named `arguments` value in
the canonical constructor. `target` must resolve to a SERVER function. The
argument value must be a checked `std.call.args` value whose parameters bind to
the target by stable `ParameterId`. A CLIENT target, a missing or duplicate
argument, a value with the wrong type, a caller-supplied principal, or an
unchecked expected result type is rejected before artifact emission.

The compiler derives `T` from the pinned target declaration. It does not accept
a client-provided result descriptor as authority. The target's declared return
type and active revision are retained in the checked client plan.

The existing CLIENT expression surface remains closed. A resource constructor
is a new checked call form with a special standard-library identity; it is not a
normal same-domain CLIENT call. Same-domain CLIENT calls continue to use the
ordinary expression call rules from work ADR 0068.

## Actions

The action language in this ADR is conceptual and is bounded for executable v1
by ADR 0079. The accepted executable v1 constructor is `std.action.call` only.
`std.action.sequence` and `std.action.parallel` are reserved and rejected
until a later scheduler contract. A constructor does not submit work. A trigger
submits a new resource request with a new request identity and generation under
the current authenticated invocation.

`std.action.call` accepts either a CLIENT target or a SERVER target. A SERVER
target uses the resource request contract. Conceptually, `sequence` starts
members in source order and stops at the first failure or cancellation, while
`parallel` starts all members, waits for terminal outcomes, and cancels
outstanding members when one fails or is cancelled. Those descriptions are not
executable v1 scheduler semantics; the two forms remain reserved and rejected
until a later scheduler contract. A completed member is not rolled back.

An action value contains no principal, role, `run_as`, capability grant, or
credential. The runtime derives the authenticated context when the action is
triggered. The compiler checks the enclosing CLIENT function's declared local
capabilities before it emits a server action call.

## State and instance context

A resource or server action is a nested operation of the current root
invocation. The checked plan retains its stable call-site identity. The runtime
supplies the parent invocation and function-instance context to the transport
adapter. The source expression cannot select a principal or replace the
inherited state profile.

The server derives USER state ownership from the authenticated session and
applies the existing `UserStateKey` and optimistic revision rules. A client
cannot select another principal by changing a resource argument, state profile,
or instance key.

## Failure and invalidation rules

Resource construction never changes the resource to `READY`. A dependency
change selects a new cache key, cancels the old generation when it is still
active, and submits a new generation. A stale completion is rejected by the
existing key, revision, type, and generation checks.

`AWAIT` does not introduce an exception tail. The current operation receives the
existing structured failure form. UI functions that need to render loading or
failure states inspect the resource value without awaiting it.

There is no automatic retry or hidden timeout. An explicit refresh or action
trigger creates a new request. A bounded root deadline may cancel a child
request, but source code cannot provide an unbounded deadline or a retry policy.

## Parser and compiler acceptance

The parser must recognise the following closed forms in CLIENT procedural bodies:

```text
resource_constructor
    := qualified_name "(" named_resource_argument "," named_resource_argument ")"

named_resource_argument
    := "target" "=>" server_invocation
    |   "arguments" "=>" call_arguments

await_expression
    := "AWAIT" resource_expression
```

The two constructor arguments may appear in either order, but each occurs once.
`server_invocation` is one qualified target with a checked argument list. The
parser retains source spans for the target, arguments, resource kind, and
`AWAIT` expression. The compiler enforces the semantic order and type rules.

The existing `LET`, assignment, and `RETURN` grammar remains the enclosing
statement contract. `AWAIT` is not accepted in a SERVER body or in an arbitrary
CLIENT expression position that can be evaluated without a suspension point.

## Artifact contract

The checked CLIENT artifact gains resource operation nodes in the next plan
version. Each node retains:

- resource kind: scalar or stream;
- target `FunctionId` and pinned `RevisionPair`;
- call-site identity;
- canonical parameter-to-expression argument pairs;
- declared result type identity;
- no principal or local grant payload.

The artifact decoder validates all identities, argument order, target domain,
result type, node limits, and plan limits before execution. The client runtime
uses the existing `ClientResourceKey` and canonical argument digest rather than
reconstructing identity from source text.

Action nodes use the same target, call-site, argument, and capability checks.
Their runtime trigger creates a resource request; no action node may contain a
transport handle or a principal.

## Required tests and proof

The language slice requires focused parser/compiler tests for:

1. canonical scalar and stream resource constructors;
2. either constructor-argument order and duplicate/missing argument rejection;
3. SERVER target resolution and CLIENT-target rejection;
4. typed argument binding and target-derived result types;
5. `AWAIT` in `LET`, assignment, and `RETURN` positions;
6. rejection of `AWAIT` in SERVER bodies, arbitrary non-suspending expressions,
   and malformed resource operands;
7. action construction without submission and action target/capability checks;
8. plan round trips that preserve call-site, target revision, argument identity,
   and resource kind without principal or grant fields.

An installed host proof must invoke one parameterised SERVER function from a
CLIENT resource and show a checked typed result. The proof must also show a
stale completion cannot update the resource and that a denied capability or
server authorisation returns the redacted error form.

## Deferred surface

Virtual models, automatic stream replay, cursor semantics, cleanup/finally
syntax, graphical event bindings, and reflective gateways remain outside this
ADR. The transport and server execution details are defined by work ADR 0078.

## Precedence

This ADR implements spec ADR 0017 and extends work ADR 0068. Work ADRs 0060,
0069, 0070, 0071, 0073, and 0074 remain authoritative for capabilities, state,
resource identity, value transport, and local lifecycle validation. The sealed
`sys.invoke` boundary remains authoritative for server execution and security.
