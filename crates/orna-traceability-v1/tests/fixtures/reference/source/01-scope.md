# 1. About this specification {#scope}

Orna is a statically typed language for programs whose code, relational data and committed history share a Git repository. A local embedded runtime owns transactions and resumable execution. Git commits identify committed database snapshots; the logical current working database, **CWD**, also includes durable changes not yet published into Git.

This specification defines the language, repository semantics, embedded runtime, system interface, storage and live-presentation profiles. It does not prescribe a compiler implementation language, a user-interface toolkit or a hosting provider.

## Conventions {#scope-document-status}

The specification comprises numbered requirements, algorithms, grammar productions, API schemas and format definitions. Examples and implementation notes are informative.

Examples specify their module context and dependencies. Signature tables use reference notation for intrinsic interfaces. Listings marked `text` contain signatures, placeholders or transcripts; listings marked `orna` contain source code.

## Using this document

The chapters progress from the language and data model to execution, repositories and interfaces. The table below provides entry points by topic. Requirement identifiers and API names link to their definitions; the [grammar](#grammar) and [diagnostic reference](#diagnostics) provide syntax and error lookups.

| To understand… | Start here | Continue with… |
|---|---|---|
| How to read and write a program | [Source and modules](#source-modules) | [Types](#types), [functions and expressions](#expressions), [worked examples](#examples) |
| What an update commits or rolls back | [Transactions and tasks](#execution) | [Assertions](#tables), [checkpoints](#checkpoints) |
| How branches relate to pending data | [Repository state](#repository) | [Branching](#branching), [publication](#publication) |
| How a value reaches disk | [Tables](#tables) | [Storage](#storage), [canonical formats](#formats) |
| How to inspect or control a runtime | [System operations](#system) | [System reference](#system-reference), [administration](#administration) |
| How a client follows live results | [Presentation](#presentation) | [Pages](#pages), [live protocol](#protocol) |
| How to implement and test a processor | [Grammar](#grammar) | [Conformance evidence](#conformance), [diagnostics](#diagnostics) |

## Normative language and consistency

The words **MUST**, **MUST NOT**, **REQUIRED**, **SHOULD**, **SHOULD NOT**, **RECOMMENDED**, **MAY** and **OPTIONAL** have the meanings specified by [BCP 14](#ref-bcp14) only when capitalised. Lowercase prose uses those words in their ordinary sense.

Numbered requirements, named algorithms, grammar productions and machine-readable schemas are normative and apply together. Informative notes and examples do not override them. Conflicting normative provisions are specification defects, not implementation-defined alternatives.

Diagnostic codes identify primary error classifications. Implementations may add explanatory notes and secondary source spans without changing which forms the language accepts.

## Conformance classes

| Class | Required scope |
|---|---|
| Language processor | Lexing, parsing, resolution, static inference/checking, ordinary evaluation, failures and assertions. |
| Repository engine | Logical CWD, Git snapshots, staging, branching, publication, identity, merge and storage validity. |
| Runtime | Activation transactions, owned tasks, streams, durable checkpoints, failure recovery and system observations. |
| CLI | The specified Git-compatible commands, Orna commands, diagnostics and machine-readable status. |
| REPL | Session module, safe inspection, completion contexts, session-owned work and presentation. |
| Server | Session establishment, trusted remote evaluation, page actions, live protocol and request recovery. |
| Renderer | Presentation tree, deterministic patch application, unsupported-node fallback and bounded rendering. |
| Codec | Typed encode/decode and the claimed canonical or external format profile. |
| Connector | Declared delivery, source identity, positions, replay, cancellation and preservation behaviour. |
| Storage profile | The logical table contract and every required encoding, integrity and recovery rule of the claimed profile. |

**ORNA-CONF-001** A conformance claim MUST identify its classes, implementation version and exact specification publication digest.

**ORNA-CONF-002** A claim MUST distinguish supported optional profiles from mandatory facilities and list the implementation tests actually executed.

**ORNA-CONF-003** An implementation MUST pass every applicable conformance fixture and behavioural test before claiming the corresponding class. A parsed example is not proof of type correctness or runtime behaviour.

**ORNA-CONF-004** Extensions MUST NOT change the meaning of valid source, repository trees or protocol messages in the claimed profile.

**ORNA-CONF-005** A language-processor claim MUST reject the invalid forms in the diagnostic corpus with the specified primary diagnostic class where the form can be identified.

**ORNA-CONF-006** A full-runtime claim MUST execute the assertion, automatic-failure, recovery, conversion, transaction, cancellation and cross-table validation scenarios applicable to it.

**ORNA-CLOSURE-004** Configuration options MUST NOT change the language, identity, transaction, checkpoint or repository semantics specified here. Resource limits and presentation preferences may vary only where explicitly permitted and must remain observable.

## Boundaries

Distributed transactions across independent databases, exactly-once effects in arbitrary external services, automatic resolution of all semantic merge conflicts and enterprise authorisation are outside this specification. Trusted-process deployments enforce the specified type safety, privacy boundaries, cancellation, integrity checks and destructive-operation preconditions.


