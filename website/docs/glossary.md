---
title: Glossary
description: Terms used across the OrnaDB documentation, defined for readers who know SQL but are new to the program model.
---

# Glossary

Terms are listed alphabetically. Each definition is one idea. Cross-references point to the page that explains the idea in context.

:::note Scope
This glossary covers the current v0.2 terms. Earlier names such as `HOST FUNCTION`, `RUNS ON`, and `CURRENT_USER` are rejected and do not appear as current usage anywhere on this site.
:::

**Action** - a typed client-side value triggered by a runtime event. An action can update state, call a CLIENT or SERVER function, or open a function invocation. See [UI and runtimes](/ui-and-runtimes/).

**Application** - an informal, user-facing term for a running program. It is not a schema object. A program is a rooted function invocation graph plus state, resources, and runtime materialisation.

**Canonical result** - the typed value a target function returns before any presentation occurs.

**CLIENT function** - a function declared with `CREATE CLIENT FUNCTION`. It executes in the sandboxed local `orna` process. See [functions](/functions/).

**Effective principal** - the principal used for a particular permission evaluation, possibly changed by a `SECURITY DEFINER` transition. See [security and inspection](/security-and-inspection/).

**Exposure** - explicit metadata that allows a reflective gateway to publish a function through a protocol. It is never implicit.

**FunctionId** - the stable semantic identity of a function, independent of its display name. A semantic rename keeps the ID.

**Inspector** - the dogfooded CLIENT function that visualises invocation, source, state, security, and runtime internals. See [security and inspection](/security-and-inspection/).

**Invocation** - execution of a pinned function revision with typed arguments and a security context. See [invocation](/invocation/).

**Object type** - a durable, identity-bearing type whose instances have `ObjectId`s. See [object model](/object-model/).

**OrnaDB** - Object-Relational Native Applications. The database and programming environment described on this site.

**orna** - the client and CLI executable. There is no released build yet. See [status](/status/).

**Presenter** - a registered function that transforms one result type into another type or surface, such as `std.terminal.Document` or a JSON byte stream. See [invocation](/invocation/).

**Principal** - a security identity. Kinds are `USER`, `ROLE`, `SERVICE`, and `EXTERNAL`. See [security and inspection](/security-and-inspection/).

**Program** - the graph rooted at an invocation, including nested calls, state, resources, presenters, and runtime materialisation.

**Resource** - a reactive client-side handle to a typed asynchronous computation, usually a SERVER function call. See [UI and runtimes](/ui-and-runtimes/).

**Revision** - an immutable version of a function or definition. A running invocation pins the revision it uses. See [architecture](/architecture/).

**Runtime** - a locally installed implementation that consumes one or more sink types. Examples are `orna-runtime-tty` and `orna-runtime-qt`. See [UI and runtimes](/ui-and-runtimes/).

**Runtime contract** - a versioned semantic external CLIENT function implemented by a runtime, such as `std.ui.window@1`.

**SERVER function** - a function declared with `CREATE SERVER FUNCTION`. It executes beside the data in the OrnaDB server. See [functions](/functions/).

**Session principal** - the authenticated principal that created the session. It comes from the trusted connection, never from a request body.

**Sink** - a type or surface the local client can consume, such as a terminal document, a byte stream, or `std.ui.UI`. See [invocation](/invocation/).

**std.ui.UI** - a standard-library transient value type describing portable semantic UI. It is not a core keyword. See [UI and runtimes](/ui-and-runtimes/).

**USER state** - durable per-principal program state, keyed server-side by the authenticated principal. The complete program-state scopes are `LOCAL`, `SESSION`, and `USER`. See [UI and runtimes](/ui-and-runtimes/).

**Value type** - a by-value type without inherent durable object identity. `TEXT` and `std.ui.UI` are examples. See [object model](/object-model/).

## Further reading

- [Getting started](/getting-started/) for the model in five minutes.
- [Object model](/object-model/) for types and references.
- [Functions](/functions/) for the executable domains.
- [Invocation](/invocation/) for the presentation path.
- [Status](/status/) for what exists today.

Return to the [OrnaDB overview](/) or inspect the [source repository](https://github.com/workingdir/ornadb).
