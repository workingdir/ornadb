# 15. The system model {#system}

## Role and namespace

`sys` is the implementation-provided model of the attached database and its execution. Unlike [the standard library](#standard-library), it is not an importable replacement package. Its rows and results are ordinary typed values; its relation handles are read-only query sources.

Use `sys.catalog` for declarations, `sys.history` and `sys.git` for retained history, `sys.rt` for live execution, `sys.storage` for physical placement, and `sys.admin` for explicit state changes. The [complete reference](#system-reference) lists every member, field, enum and signature. The [administrative contract](#administration) defines mutation preconditions.

The familiar query form works for inspection:

```orna
sys.catalog.objects
    | map(object => object.qualified_name)
```

This example requires only a database with catalogue metadata. It returns semantic object names, not executable function values. As with application relations, an explicit sort is needed when output order matters.

**ORNA-SYS-001** Structural references within `sys` MUST use typed references where possible, not unstructured path strings.

**ORNA-SYS-002** Every Orna 1.0 implementation MUST provide the complete portable `sys` surface applicable to each conformance class it claims.

**ORNA-SYS-003** User source MUST NOT declare, replace, shadow or monkey-patch the root `sys` namespace or any portable member specified in this chapter.

**ORNA-SYS-004** A vendor extension under `sys.vendor.<vendor>` MUST NOT change portable row identity, field meaning, ordering, failure codes, transaction boundaries or redaction rules.

**ORNA-SYS-005** The unrecognised member `sys.runtime` MUST produce `ORNA100-E-SYS-RUNTIME` with `sys.rt` as the suggested spelling. It is not an alias.

**ORNA-SYS-010** Portable names are case-sensitive. `sys.Table`, `sys.catalog.tables` and `sys.table` are not interchangeable spellings unless this specification explicitly defines them.

## Compatibility and observations

`sys.rt.info()` returns the exact coordinates declared by `sys.RuntimeInfo`. The selected `std` package is identified by its retained source snapshot, not by assuming that every repository uses the same package release. Unicode and timezone-data coordinates are reported separately where relevant.

A system query can inspect one of five kinds of data:

| Availability | Observation and lifetime |
|---|---|
| Catalogue | Declarations and semantic metadata of the pinned source snapshot. |
| Durable observation | A runtime event deliberately retained in local state or a published snapshot. |
| Local durable | Present in the logical CWD, not necessarily in a Git commit. |
| Live | Current owner/session/operation state; invalid outside that runtime generation. |
| Derived | Computed from available source observations, preserving their pin and redaction. |

A row in an old snapshot saying that a run was running is a historical observation, not evidence that the run is alive now. The runtime view contains only its current live subset. A missing blob is not the same as a known empty relation.

**ORNA-SYS-006** A conforming runtime MUST expose the compatibility fields of `sys.RuntimeInfo` in the [system reference](#system-reference), including exact language, system API, codec, storage and protocol coordinates; it MUST NOT claim a coordinate while omitting its required surface.

**ORNA-SYS-007** Unknown additive fields from a newer compatible `sys` minor version MUST be preserved or ignored safely according to the consumer's decoding mode; they MUST NOT be interpreted as a known field with a different meaning.

**ORNA-SYS-008** A consumer requiring an unavailable `sys` coordinate or profile MUST fail before performing writes.

**ORNA-SYS-009** A grouped catalogue/history handle and its canonical PascalCase relation MUST identify the same rows at the same snapshot. A `sys.rt` handle is the current-runtime live subset of its canonical relation, not a promise that all retained historical rows are live.

**ORNA-SYS-011** A historical system query MUST be snapshot-pinned and MUST NOT silently resolve any referenced object against a newer CWD or `HEAD`.

**ORNA-SYS-012** A historical durable-system query MUST return exactly the observations retained by its snapshot. Known absence is an empty relation; pruned, missing or unhydrated data MUST be reported as unavailable, never substituted with observed emptiness.

**ORNA-SYS-013** A live `sys.rt` relation MUST NOT accept `as_of`. The implementation MUST direct callers to the corresponding durable relation when one exists.

**ORNA-SYS-014** CWD system relations MAY overlay newer local durable observations from Turso, but each row MUST identify whether it is committed, local-only or live-derived.

**ORNA-SYS-015** A historical row that states a run was `running` records the state observed at that snapshot; it MUST NOT imply that the process remains alive.

**ORNA-SYS-022** `sys.database.cwd` MUST remain stable for one evaluation snapshot and MUST change only at a defined transaction/snapshot boundary.

**ORNA-SYS-023** `sys.database.writable` reports whether the current attachment permits Orna-controlled writes; it does not assert that every later write will succeed.

**ORNA-SYS-024** Every function activation MUST observe one stable `sys.current.snapshot` unless it explicitly opens a nested operation whose API documents a new snapshot.

**ORNA-SYS-027** `sys.rt.id` identifies the current local owner/runtime generation and MUST change after an owner restart that invalidates prior live handles.

**ORNA-SYS-028** `sys.rt` MUST be available to every Orna command that opens a database, including direct embedded commands and commands temporarily coordinated through a local owner process.

**ORNA-SYS-029** `orna serve` MUST NOT be treated as the unique or authoritative `sys.rt`; it is one optional client/host mode over the same embedded-first database model.

**ORNA-SYS-030** Reads from `sys.rt` MUST be internally consistent to one observation instant or expose an `observed_at` field allowing the caller to detect a mixed observation.

## Identity, references and context

An object identity answers “which declaration?”; a revision identity answers “which immutable definition?”; a snapshot answers “which database state?”. A reference keeps all context necessary to resolve its target. Renaming a declaration does not casually turn it into an unrelated object.

Every canonical system row has a read-only `.reference`. The reference is not the row itself and is not a mutable proxy. Ordinary source obtains it explicitly. A `sys.FunctionRef` is likewise not a closure and a `sys.ConsumerIdentity` is not a function name.

`sys.current` describes the current activation: snapshot, optional transaction/invocation/run/session/client, locale, timezone, trace and cancellation context. Optional fields use `null` for absence; no fabricated zero identifier stands for “not present”. Singleton views are immutable observations, refreshed only at specified boundaries.

[Snapshot encoding](#formats) distinguishes committed snapshots from exact CWD generations. Serialising a live handle produces a non-operational description. It does not grant the ability to resume an old task after a process restart.

**ORNA-SYS-016** Opaque system identifiers MUST compare by the identity they name, MUST have deterministic canonical encoding and MUST NOT be implicitly interchangeable with `Str`, `Uuid` or another identifier type.

**ORNA-SYS-017** A stable `sys.ObjectId` identifies one semantic object across ordinary edits and semantic renames; a `sys.RevisionId` identifies one immutable revision of that object.

**ORNA-SYS-018** Reusing an identifier for a semantically unrelated object is repository corruption and MUST be diagnosed by verification before publication.

**ORNA-SYS-019** Dereferencing a snapshot-pinned reference MUST use its pinned snapshot unless the caller explicitly asks to re-resolve by stable object identity in another snapshot.

**ORNA-SYS-020** A live handle presented to a different runtime owner MUST fail with `sys.handle.foreign_runtime`; it MUST NOT be treated as an identifier for a coincidentally equal live object.

**ORNA-SYS-021** Canonical serialization of a live handle MUST mark it non-resumable. Decoding it in a later process yields descriptive data, not an operational handle.

**ORNA-SYS-025** Child calls inherit locale, timezone, trace and cancellation context unless an explicit API parameter replaces one of them.

**ORNA-SYS-026** `sys.current.transaction` is present only when the activation participates in an Orna transaction. Its absence MUST NOT be represented by a fabricated zero identifier.

**ORNA-SYS-111** Every canonical system-relation row MUST expose a read-only `reference` property whose type is the generated alias for that row type.

**ORNA-SYS-112** A row reference MUST encode or otherwise preserve the row's natural key and resolution context; for a naturally keyed row such as `sys.Checkpoint` or `sys.Failure`, it MUST NOT introduce a synthetic identifier.

**ORNA-SYS-113** A row value MUST NOT be implicitly converted to a row reference or vice versa. Source uses `row.reference` to obtain a reference and an applicable relation/function to dereference it.

**ORNA-SYS-114** Possession of a reference MUST NOT bypass visibility, retention, redaction, trust, transaction or administrative-precondition checks.

**ORNA-SYS-135** Source spans and maps MUST retain their pinned file/snapshot context and canonical UTF-8 coordinates through diagnostics, source lookup, blame and presentation.

**ORNA-SYS-136** Snapshot selection is represented by explicit overloads for `SnapshotRef`, `CommitRef`, `BranchRef`, `TagRef`, `GitOid` and `Str`; Orna 1.0 defines no implicit `SnapshotLike` union or coercion type.

**ORNA-SYS-137** Singleton view values MUST be immutable observations. Administrative state MUST NOT be changed by field assignment or by mutating a collection obtained from a view.

## Catalogue and dependency graph

The catalogue records the fully resolved meaning of source: modules, objects, revisions, fields, parameters, return types, protocols, implementations, table keys, assertions, dimensions and currencies. Inference changes how much users write, not how much metadata implementers publish.

Source identity and semantic identity are separate. Exact source bytes include formatting; semantic hashes omit irrelevant formatting but include all observable declaration meaning. A catalogue row states whether an annotation was authored or inferred.

Dependency edges record their kind and confidence. A table assertion's dependencies are the relations it validates, not only the tables mentioned by spelling in its immediate expression. Dynamic dependencies discovered while evaluating a query are attributed to that activation.

Traversal uses a visited set keyed by object identity and snapshot. `direct` visits one edge layer. `transitive` traverses until no new objects remain, retains the shortest discovered depth, and breaks equal-depth ordering by canonical object identity and edge kind. It never loops on recursive functions or mutually referring types. Exact edge categories and fields are in `sys.Dependency`.

**ORNA-SYS-031** A state transition MUST follow the state machine specified for its row type; direct assignment of a state enum is not an administrative API.

**ORNA-SYS-032** A terminal invocation or run MUST NOT become non-terminal under the same execution identity; another execution has a new invocation/run identity. A durable failed-delivery identity follows the separate, versioned [delivery state machine](#checkpoints) and is preserved across attempts.

**ORNA-SYS-033** `qualified_name` is the name at the row's snapshot, not a timeless property of the stable object identity.

**ORNA-SYS-034** `semantic_hash` MUST exclude non-semantic formatting while `source_hash`, when present, identifies exact retained source bytes.

**ORNA-SYS-036** A generated definition MUST identify its generator/provenance where retained and MUST NOT be misreported as authored source.

**ORNA-SYS-037** Inferred types and inferred failure sets MUST be published in catalogue metadata exactly as if they had been written explicitly.

**ORNA-SYS-038** Catalogue metadata MUST retain whether an annotation was explicit so semantic tools can distinguish an annotation edit from an actual signature change.

**ORNA-SYS-039** Effect metadata MUST be a conservative superset of effects the function may perform; it MUST NOT omit a possible write, external effect or failure merely because an optimiser removed it in one build.

**ORNA-SYS-040** `sys.Table.assertions` MUST include every owner-scoped table assertion in source order and MUST expose the complete relation dependencies used for validation.

**ORNA-SYS-041** `sys.Assertion` MUST represent the contextual `assert` declarations specified by [assertions](#tables). A processor MUST NOT invent separate portable constraint/check/ensure/fact object kinds.

**ORNA-SYS-042** A table row count marked inexact MUST never be used as if it were an exact integrity fact.

**ORNA-SYS-043** Dependency traversal MUST be cycle-safe and deterministic.

**ORNA-SYS-044** A statically proven edge MUST use `exact` confidence. A conservative dynamic edge MAY use `possible`; tooling MUST NOT present `possible` as a proven call or write.

**ORNA-SYS-045** `sys.dependencies` and `sys.dependents` MUST preserve edge kind and source evidence rather than return only a set of names.

**ORNA-SYS-046** Diagnostic codes, primary spans and structured causes MUST remain available independently of a particular terminal or UI rendering.

## Sources, diagnostics and redaction {#system-redaction}

`sys.source` returns an authored or generated `sys.SourceDocument` pinned to its file and snapshot. `sys.describe` returns structured metadata. Neither is a request to reveal credentials, arbitrary host paths or unretained historical source.

Coordinates use zero-based UTF-8 byte offsets and half-open `[start, end)` spans. Lines and columns presented to humans are derived from those bytes; tab width is a renderer decision, not a source-map coordinate. Generated source identifies its generator when that information is retained.

A diagnostic has a stable code, severity, safe message, source spans, optional notes and causal references. Localisation changes the presentation, not the code or causal identity. Redaction happens before a value crosses a system, trace or codec boundary. A protected value retains a safe explicit marker; it is not replaced with plausible fake text.

**ORNA-SYS-035** Source paths returned outside explicitly trusted developer inspection MUST be repository-relative or redacted according to [redaction rules](#system-redaction).

**ORNA-SYS-047** Rendering a diagnostic MUST NOT mutate it, change its code or alter the underlying failure identity.

**ORNA-SYS-048** Secret values, decrypted source fragments, authorization material and unredacted connector payloads MUST NOT appear in diagnostic `message`, labels, causes, help, structured data, traces or presentation fallbacks.

**ORNA-SYS-049** A Git commit row and a semantic snapshot row MUST remain distinguishable even when they correspond one-to-one.

**ORNA-SYS-050** Hidden Orna refs MUST be visible through `sys.Ref.hidden` in explicit developer inspection and MUST remain valid ordinary Git refs.

**ORNA-SYS-051** A semantic diff MUST classify rename and rekey when identity evidence proves them; otherwise it MUST report conservative delete/add rather than invent identity.

**ORNA-SYS-052** Missing promisor objects MUST be represented explicitly. Introspection MUST NOT fabricate unavailable source, rows or statistics.

**ORNA-SYS-070** Storage locations and host paths are sensitive metadata and MUST follow the path-redaction rules.

**ORNA-SYS-071** `sys.Secret` MUST never expose decrypted secret bytes, derived authorization headers or a value formatter that reveals them.

**ORNA-SYS-096** A portable failure code MUST retain its meaning throughout the 1.x compatibility line.

**ORNA-SYS-097** Implementations MAY add structured fields and vendor causes, but MUST NOT replace a required portable primary code with a vendor-only code.

**ORNA-SYS-098** Failure messages may be localized; code, structured fields and causal identity remain stable.

**ORNA-SYS-099** Redaction MUST occur at the system-value boundary and MUST NOT depend solely on a renderer remembering to hide a value.

**ORNA-SYS-100** A redacted value MUST preserve safe type/identity metadata and an explicit redaction marker; it MUST NOT substitute plausible fake plaintext.

**ORNA-SYS-101** Canonical codecs MUST refuse to encode a protected value in reveal mode unless an explicitly trusted API outside generic `sys` grants that operation. No portable generic reveal API exists in 1.0.

**ORNA-SYS-102** Hashes of low-entropy secret values MUST be treated as sensitive. Domain separation alone does not make a dictionary-attackable hash safe; a public identifier MUST NOT reveal such a hash.

## History, plans and storage

`sys.snapshot` resolves a supported selector to a pinned `sys.SnapshotRef`. A string selector is resolved once; moving its branch later does not change the returned reference. Root analysis functions read metadata without switching branches or creating commits.

`sys.diff` compares semantic snapshots. `sys.changes` describes changes within the selected context. A `sys.DiffEntry` wraps its `change`; the object belongs to `entry.change.object`, not an invented flattened field. `sys.Commit` has distinct `authored_at` and `committed_at` fields.

`sys.explain` returns a plan reference with structured nodes and dependency metadata. A `sys.Query` can refer to a plan; a plan does not invent an inverse `.query` field. An optimiser may change the plan while preserving results and observable ordering.

Storage metadata distinguishes an observed profile, a future placement preference, and an explicit rewrite target. Changing a preference is not a physical rewrite. A rewrite preserves keys, logical values, ordering and canonical row meaning, so its semantic row diff is empty. [Storage profiles](#storage) define representability and format limits.

**ORNA-SYS-053** Explain output MUST be structured `sys.Plan` data. Human-readable tables or trees are `Present` renderings and are not the semantic result.

**ORNA-SYS-054** Actual execution counters MUST be absent when not measured; they MUST NOT be populated with estimates while marked actual.

**ORNA-SYS-055** A plan is snapshot-specific and MUST NOT be reused against a different schema revision without revalidation.

**ORNA-SYS-056** Every reflective `sys.invoke` or `sys.start` call MUST create an invocation identity before user code begins.

**ORNA-SYS-057** An unhandled ordinary failure MUST mark the invocation failed and roll back its Orna-controlled transaction before the failure is reported as terminal.

**ORNA-SYS-058** Cancellation MUST be recorded separately from ordinary failure and MUST bypass `|?` recovery inside the cancelled activation.

**ORNA-SYS-059** A run's `live` field is meaningful only for the current observation and MUST be false or absent in a purely historical projection.

**ORNA-SYS-060** Trace retention MAY be bounded, but the absence of discarded trace detail MUST be explicit and MUST NOT change invocation outcome.

**ORNA-SYS-067** Physical storage rows are introspection data and MUST NOT change the logical contents, ordering, equality or serialization of table rows.

**ORNA-SYS-068** A compact segment MUST identify its schema, key range, generation and content digest sufficiently to detect stale or overlapping publication.

**ORNA-SYS-069** Missing lazily hydrated objects MUST be exposed as unavailable; a query may hydrate them according to policy but MUST NOT silently substitute incomplete results.

**ORNA-SYS-072** Build metadata MUST identify the complete source snapshot and compatibility coordinates used to produce an artifact.

**ORNA-SYS-073** A specification fixture listed as planned evidence MUST NOT be represented as passed unless an implementation actually executed it.

**ORNA-SYS-074** Every read/analysis function MUST return a normal typed value or relation that can be queried, encoded where supported and presented independently.

**ORNA-SYS-075** A root analysis function MUST NOT change checkout, refs, table contents, checkpoints, process state or retained failure state.

**ORNA-SYS-076** Name resolution and history functions MUST respect visibility and source redaction even in a trusted deployment.

**ORNA-SYS-115** `sys.Storage.profile` MUST describe observed placement and `sys.Storage.preference` MUST describe future automatic placement policy; implementations MUST NOT conflate the two.

**ORNA-SYS-116** Setting storage preference MUST NOT rewrite existing logical rows or produce a semantic row change.

**ORNA-SYS-117** A storage rewrite MUST preserve every logical key/value, ordering and canonical row meaning, and its semantic row diff MUST be empty.

**ORNA-SYS-118** A rewrite to editable storage MUST fail before publication with `sys.storage.unrepresentable_key` when any row key cannot be represented by the canonical path codec or when the normative editable resource bounds cannot be met.

**ORNA-SYS-119** A rewrite collision, stale generation or concurrently changed input MUST fail with `sys.storage.rewrite_conflict` and leave the prior generation authoritative.

**ORNA-SYS-120** `sys.storage` is a grouping namespace, not a callable selector. Portable source MUST use `sys.Storage`, `sys.storage.objects` and the explicit `sys.admin` storage functions.

## Reflective invocation

Reflection is typed invocation, not source evaluation. The result witness `as: T` selects a typed boundary; `sys.Value` is an explicit type-preserving envelope, not an inference fallback or an implicit conversion chain.

A `sys.ArgumentMap` accepts named arguments at the reflective call boundary, retains each exact originating type, rejects duplicate names, and orders names by canonical UTF-8 for identity hashing. The normal source record is admitted here only by the explicit reflective parameter contract; arbitrary records do not silently become `sys.Value` elsewhere.

### Algorithm INVOKE-1

1. Resolve the `sys.FunctionRef` in its pinned snapshot. If `at` is `null`, use that pin—not the caller's current CWD. An explicit `at` must identify the same pinned snapshot; otherwise fail with `sys.invoke.snapshot_mismatch`. To select another revision, explicitly resolve the function in that snapshot before invoking it.
2. Check visibility and that the declaration is callable. Reject unresolved generic arguments and bound values whose static types do not match the signature.
3. Bind each named argument once. Apply declared defaults in the function's pinned context, reject unknown/missing names, and validate the explicit result witness before any target effect.
4. Select the documented execution mode. An inherited transaction is permitted only when its snapshot agrees with the target and the operation is synchronous. An asynchronous start uses a separate transaction or read-only mode, never a transaction whose owner can complete before it.
5. Capture locale, timezone, trace and cancellation ownership. Compute the identity tuple from function identity, revision, snapshot, typed bound arguments including defaults, expected return type, mode and relevant context.
6. Atomically admit the idempotency key, where supplied. A retained entry with a different tuple fails; an identical terminal entry returns its retained outcome. An identical active entry refers to that same operation, not a second execution. Missing/pruned outcome information is not evidence that re-execution is safe.
7. Create the invocation observation, run the target and record one terminal outcome. Commit owned Orna writes only on successful completion; otherwise roll them back. Follow [child termination](#execution) before declaring the owner terminal.
8. Publish a redacted result/diagnostic and retained trace state. A crash after an external side effect does not permit a false exactly-once guarantee.

`sys.start` returns a typed handle. `sys.await` returns one complete `sys.InvocationResult<T>` or raises `sys.invoke.await_timeout`; a timeout does not cancel or detach the target. `sys.cancel` acts on an invocation handle, not a durable-run reference. [Task ownership](#execution) determines whether an invocation belongs to an operation or a REPL session.

### Result invariants

A successful `sys.InvocationResult<T>` has a present value and no ordinary failure. A failed result has a failure and no value. A cancelled or orphaned result has no successful value. It may carry a diagnostic explaining termination, but that diagnostic does not turn cancellation into an ordinary catchable failure inside the cancelled target. The result never represents a running or timed-out target. A timed-out wait returns no partial result.

An `Option<T>` result may itself be `null`. The outer presence flag in the invocation result must preserve that distinction; `Some(null)` is a successful optional result, not an absent result. [Canonical values](#formats) preserve nested options.

**ORNA-SYS-077** `sys.invoke` MUST NOT evaluate arbitrary source text or bypass the parser, resolver, type checker, visibility rules, effect rules or transaction model.

**ORNA-SYS-078** An argument envelope MUST NOT allow a value to masquerade as another static type merely because its data representation is compatible.

**ORNA-SYS-079** An idempotency key binds to function identity, exact revision, snapshot, expected result type, canonical typed arguments, invocation mode and relevant context. Reuse for a different tuple MUST fail with `sys.invoke.idempotency_mismatch`.

**ORNA-SYS-080** Returning a cached idempotent success is permitted only after the implementation proves the complete identity tuple matches and the retained result is valid under the same codec/type version.

**ORNA-SYS-081** `sys.start` MUST return a handle bound to one runtime generation and one invocation identity.

**ORNA-SYS-082** `sys.await` timeout MUST NOT cancel, detach, reparent or otherwise change the target invocation.

**ORNA-SYS-083** `sys.cancel` MUST be idempotent and MUST NOT convert cancellation into an ordinary recoverable failure inside the target.

**ORNA-SYS-129** Every type named by a portable `sys` field or function signature MUST be a language type, a core presentation/query type, a canonical system relation, an opaque identifier, a closed enum or a supporting value type enumerated by `api/sys.json`; undocumented pseudo-types are forbidden.

**ORNA-SYS-130** `sys.Value` MUST NOT be used as an implicit inference fallback or an implicit conversion route. It MUST preserve exact originating type identity and MUST require an explicit reflective API to unbox or validate it.

**ORNA-SYS-131** `sys.ArgumentMap` MUST reject duplicate names, preserve each argument's exact originating type and produce a deterministic canonical name order for idempotency hashing.

**ORNA-SYS-132** The typed `sys.invoke<T>` and `sys.start<T>` overloads MUST validate the function's declared result against the explicit `as: T` witness before any function effect occurs. A mismatch MUST fail with `sys.invoke.return_type`; no conversion chain is attempted.

**ORNA-SYS-133** `sys.InvocationResult<T>` MUST describe one complete terminal outcome and MUST satisfy the value/failure invariants stated above.

**ORNA-SYS-134** `sys.CheckpointPosition` MUST remain opaque outside its provider contract; generic code MUST NOT order it, increment it or synthesize a successor.

**ORNA-SYS-138** A conforming implementation MUST expose the supporting-type fields and invariants in `api/sys.json` and MUST reject a portable API claim whose type graph contains an unresolved name.

## Mutation and observation consistency

Portable system rows are read-only. Setters, row mutators and collection mutation cannot bypass the [administrative operations](#administration).

Each administrative command records an invocation and safe outcome. Commands spanning Git and local database state use their specified journal/recovery protocol; the word “atomic” does not imply a hidden transaction shared by arbitrary Git processes and an external server.

Multi-relation inspection pins one snapshot or observation instant. A continuation token binds the snapshot, filter, order and redaction context; it cannot be used to splice results from another generation. Relation order remains unspecified unless explicitly ordered.

Retention distinguishes known absence from unavailable detail. Losing optional traces cannot invalidate an advertised complete committed database snapshot, and reading history never restarts an old program.

**ORNA-SYS-084** Direct mutation of a portable `sys` relation MUST fail at resolve/type-check time where statically evident and otherwise before any write is performed.

**ORNA-SYS-085** Every administrative function MUST define its transaction boundary, compare-and-set preconditions, failure codes and durable audit row.

**ORNA-SYS-086** An administrative function MUST validate all affected assertions before publishing its state transition.

**ORNA-SYS-087** A failed administrative operation MUST leave repository refs, logical CWD, checkpoints and runtime state at the last valid boundary described by its algorithm.

**ORNA-SYS-088** The 1.0 `sys.admin` boundary relies on OS/process isolation, Git/SSH credentials and the authenticated network perimeter; it MUST NOT be misrepresented as a built-in enterprise grants or role engine.

**ORNA-SYS-089** A continuation token MUST NOT be reused against a different snapshot, order, filter or redaction context.

**ORNA-SYS-090** Default iteration over an unordered system relation MUST NOT be documented as stable merely because one implementation happens to use object-ID order.

**ORNA-SYS-091** A multi-relation inspection operation requiring a coherent view MUST pin one observation snapshot/instant and expose it in the result.

**ORNA-SYS-092** A client that detects an observation-generation change during a live pagination sequence MUST restart or explicitly accept a new generation; rows MUST NOT be silently spliced across generations.

**ORNA-SYS-093** Retention policy MUST NOT delete data required to reproduce a committed logical snapshot while that snapshot remains advertised as complete.

**ORNA-SYS-094** Pruned optional detail MUST be represented as unavailable metadata, not as an empty value that could be mistaken for observed emptiness.

**ORNA-SYS-095** Retention and publication of runtime observations MUST never cause external effects or restart a program merely because a historical relation is queried.

## Conformance obligations

The complete schema is published both in the [system reference](#system-reference) and `api/sys.json`. Generated declarations are signature notation, not an independent redefinition of the language grammar. A disagreement between those representations is a specification defect, not implementation freedom.

The requirements below specify implementation evidence. The package's authoring checks and semantic models cover only their explicitly reported scope; they are not a declaration that a completed Orna engine has executed all of these obligations.

**ORNA-SYS-103** A conformance claim MUST list every unavailable optional retention/detail profile but MUST NOT call a required portable member optional.

**ORNA-SYS-104** The conformance suite MUST test every root function with success, not-found/wrong-kind, snapshot, redaction and cancellation/failure cases applicable to that function.

**ORNA-SYS-105** The suite MUST verify that active valid examples use `sys.rt` and that `sys.runtime` appears only as an explicitly invalid name or diagnostic subject.

**ORNA-SYS-106** The suite MUST prove live handles fail across runtime generations and that snapshot references remain stable across CWD changes.

**ORNA-SYS-107** The suite MUST prove `sys.await` timeout leaves the target running and independently cancellable.

**ORNA-SYS-108** The suite MUST prove checkpoint compare-and-set, stream-write atomicity, failure retry/skip transitions and assertion validation under injected crashes and cancellation.

**ORNA-SYS-109** The suite MUST prove secret and path redaction before Inspect, Display, Present, codec and trace boundaries.

**ORNA-SYS-110** The suite MUST compare grouped handles with the same-snapshot canonical relations, applying the documented current-runtime/live filter to `sys.rt` handles rather than comparing them with all historical rows.

**ORNA-SYS-125** The suite MUST prove every canonical relation row exposes the correctly typed snapshot/runtime-pinned `reference`, including natural-key references that introduce no synthetic identity.

**ORNA-SYS-126** The suite MUST prove storage preference does not rewrite existing rows, storage rewrite produces an empty semantic row diff, unrepresentable editable keys fail before publication and stale generations leave the prior representation authoritative.

**ORNA-SYS-127** The suite MUST verify [failure administration](#checkpoints), including stable delivery identity, versioned retries, skip, replay and resolve, and MUST reject system-row mutator methods.

**ORNA-SYS-128** The suite MUST test both `sys.source` and `sys.history` overloads with current, renamed, historical, missing, partially cloned and redacted files/objects.

**ORNA-SYS-139** The suite MUST exercise every snapshot-selector overload and prove that the returned `SnapshotRef` is pinned even when a named branch later moves.

**ORNA-SYS-140** The suite MUST exercise erased and typed invocation/start overloads, reject a mismatched `as: T` before effects, and prove `sys.Value` is never selected by ordinary failed inference.

**ORNA-SYS-141** The suite MUST validate every supporting value-type field, invariant and canonical encoding entry against `api/sys.json`.

