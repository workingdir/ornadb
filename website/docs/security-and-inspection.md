---
title: Security and inspection
description: Principals, sessions, grants, capabilities, USER state segregation, and the dogfooded Inspector.
---

# Security and inspection

Security is a first-class subsystem, not a footnote used only for UI state. Authorised users can inspect its structure and decisions through typed catalog relations and functions. Credentials and classified values remain protected or redacted.

:::warning Development status
The trust model is LOCKED. The local `std.security` administration-function boundary and bounded headless Inspector v1 are accepted slices. The security catalog schema and full security/DBA console are CURRENT PROPOSAL (CONCEPTUAL); policy, definer, and delegation semantics remain OPEN. OrnaDB is under active development; there is no released executable yet.
:::

## Principals

A principal is a security identity. Kinds are `USER`, `ROLE`, `SERVICE`, and `EXTERNAL` for possible federated identity mapping.

```text
sys.security.principal
    id
    name
    kind
    status
    owner
    attributes
    created_at
```

A database principal is not automatically a business `people.person` object. Applications create explicit mappings:

```sql
CREATE TYPE crm.agent AS OBJECT (
    person    REF people.person NOT NULL,
    principal REF sys.security.principal UNIQUE
);
```

## Session identity functions

There is no `CURRENT_USER` keyword. Use ordinary reflected security functions:

```sql
sys.security.session_principal()
sys.security.effective_principal()
sys.security.active_roles()
sys.security.current_session()
sys.security.has_privilege(...)
```

A default can reference them:

```sql
p_principal REF sys.security.principal
    DEFAULT sys.security.session_principal()
```

## DDL sugar

Familiar statements are CURRENT PROPOSAL (CONCEPTUAL) sugar that would lower to protected `sys.security.*` operations:
> **CURRENT PROPOSAL (CONCEPTUAL):** `CREATE USER`, `CREATE ROLE`, and `GRANT` syntax is unresolved DDL sugar, not a released parser contract. The accepted local administrative boundary is ordinary `std.security` administration functions; enforcement remains in the protected `sys.security.*` kernel/trusted path.


```sql
CREATE USER bob;
CREATE ROLE developer;
GRANT developer TO bob;

GRANT EXECUTE
    ON FUNCTION tasks.overdue
    TO developer;

REVOKE EXECUTE
    ON FUNCTION tasks.overdue
    FROM developer;
```

The accepted local `std.security` administration functions remain available as the narrow administration boundary; this conceptual DDL does not turn the unresolved security console or catalog schema into a released feature.


## Credential enrolment

Never put credentials in source:

```sql
CREATE USER bob IDENTIFIED BY 'secret';   -- rejected
```

Source files, traces, shell history, and audit payloads must not contain plaintext credentials. Use a protected input channel. The exact subcommand names are open:

```bash
printf '%s' "$PASSWORD" | orna user credential add bob --password-stdin
```

## Function security mode

The accepted default is `SECURITY INVOKER`. `SECURITY DEFINER` and its policy/delegation semantics remain CURRENT PROPOSAL/OPEN; the following shows only the conceptual shape.

```sql
CREATE SERVER FUNCTION security.rotate_key(...)
RETURNS VOID
SECURITY DEFINER
IS
BEGIN
    ...
END;
```

Definer functions require a fixed owner, resolved dependencies, explicit grants, audit events, and Inspector visibility of principal transitions.

## CLIENT capabilities

A remote database may provide CLIENT code, but that code receives no ambient local authority. Capabilities might include:

```text
std.fs.read(path-scope)
std.fs.write(path-scope)
std.net.listen(address-scope)
std.net.connect(host-scope)
std.process.spawn(command-scope)
std.secret.use(secret-id)
```

Capability grants are local client policy plus function declarations. Native runtimes are separately installed and trusted.

## sys.invoke security flow

```text
authenticated session principal
    -> resolve target
    -> check EXECUTE and policies
    -> evaluate SECURITY INVOKER or DEFINER
    -> check CLIENT and server capabilities
    -> execute
    -> audit and inspect decisions
```

The request cannot choose an arbitrary principal. Credentials and principal IDs come from the authenticated connection, never from the request body.

## USER state segregation

The server keys `USER` state with `session_principal()`. Normal clients cannot read or write another principal's state. Administrative inspection requires an explicit privilege and redaction.

## Auditing

Audit at minimum:

```text
login and session changes
principal, credential, and provider changes
role and grant changes
SECURITY DEFINER execution
sensitive function invocation
source and revision apply
capability decisions
protocol exposure changes
delegation and impersonation
inspection of protected values
```

## The Inspector

The accepted headless Inspector v1 is an ordinary CLIENT function returning `std.ui.UI`:

```sql
CREATE CLIENT FUNCTION devtools.inspector (
    p_target REF sys.inspect.invocation
)
RETURNS std.ui.UI
IS
    STATE selected REF sys.inspect.object
        SCOPE SESSION
        DEFAULT NULL;

    STATE active_tab TEXT
        SCOPE USER
        DEFAULT 'overview';
BEGIN
    RETURN devtools.inspector_shell(
        target     => p_target,
        selected   => selected,
        on_select  => selected.SET,
        active_tab => active_tab,
        on_tab     => active_tab.SET
    );
END;
```

It uses public introspection APIs. Required projections include:

```text
sys.inspect.snapshot(invocation)
sys.inspect.invocation_nodes(snapshot)
sys.inspect.calls(snapshot)
sys.inspect.state_cells(snapshot)
sys.inspect.resources(snapshot)
sys.inspect.ui_nodes(snapshot)
sys.inspect.presentation_plan(snapshot)
sys.inspect.security_decisions(snapshot)
sys.inspect.runtime_bindings(snapshot)
sys.inspect.trace(invocation)
```

Access is privilege-controlled. Values are redacted by classification.

## Self-inspection

Every Inspector is just another invocation:
The first target in the example may be a conceptual `studio.main()`; the recursive Inspector behavior is the accepted boundary:


```text
inv:100  studio.main()              (CONCEPTUAL full Studio target)
inv:101  devtools.inspector(p_target => inv:100)
inv:102  devtools.inspector(p_target => inv:101)
inv:103  devtools.inspector(p_target => inv:102)
```

The introspection plane publishes immutable snapshot epochs. An Inspector renders snapshot N. Effects caused by rendering it appear only in N+1, so self-observation cannot become a feedback loop.

## Next steps

- Read the executable domains in [functions](/functions/).
- Read the process topology in [architecture](/architecture/).
- Check what exists today on the [status page](/status/).

Return to the [OrnaDB overview](/) or inspect the [source repository](https://github.com/workingdir/ornadb).
