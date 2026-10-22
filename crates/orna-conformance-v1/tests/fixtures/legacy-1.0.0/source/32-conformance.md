# 32. Conformance cases and evidence {#conformance}

## Test corpus {#conformance-conventional-conformance-tests}

The conformance corpus contains source fixtures, complete projects, behavioural scenarios and value vectors:

```text
examples/valid/      modules/rows that must parse, resolve and type-check
examples/invalid/    source that must fail in the stated phase with the stated diagnostic
examples/reference/       complete reference database project
tests/               language-independent cases and vector oracles
tools/               selected executable reference checks
```

A generated JSON index describes the fixtures and their expected outcomes for use by implementation test runners.

**ORNA-TEST-001** Every valid language fixture requires successful parse, name resolution and type checking. Parser-only success MUST NOT be presented as full validity.

**ORNA-TEST-002** Every invalid fixture identifies the expected failing phase and stable diagnostic code.

**ORNA-TEST-003** The reference `examples/reference/` project is loaded as one complete database project: parse reachable modules, resolve/type-check them with its declared intrinsic environment and supplied modules, discover every loose row unit belonging to reachable tables, reconstruct keys from paths, and validate row fields/units against table schemas.

**ORNA-TEST-004** Bundle validation distinguishes: a test is specified, a test exists in an implementation, and a test passed. Document/index checks MUST NOT claim that Orna source executed when no implementation was run.

**ORNA-TEST-005** The corpus covers each normative syntax alternative, precedence boundary and deliberately invalid ambiguous form. It need not manufacture a positive and negative file for every mechanical EBNF helper production.

**ORNA-TEST-006** A generated requirement-to-test coverage report SHOULD expose untested normative behaviour, but it MUST remain a reporting layer over ordinary tests rather than a new test language or CI system. Informative rationale is excluded from the denominator.

**ORNA-TEST-009** `tests/conformance-manifest.json` MUST index valid, invalid and complete-project fixtures. A complete-project fixture MUST include `load_rows`, which validates every discovered loose row against its resolved table schema and units.

**ORNA-TEST-010** `tests/requirement-evidence.json` MUST attach a non-empty `tests` list to every numbered requirement. Entries may name ordinary source fixtures, project fixtures, vector suites, behavioural/fault scenarios, interoperability suites, benchmark suites or an explicit inspection requirement. The mapping is a test plan, not evidence that an implementation has executed it.

**ORNA-TEST-011** Machine-testable normative behaviour MUST be identified as requiring an executable fixture, vector, project, scenario or implementation suite. A plan without an implemented test remains unexecuted. Bundle validation MUST fail when a requirement has no recorded evidence obligation; production conformance additionally requires the applicable implementation evidence to pass.

## Branch-based isolation

A branch provides a natural isolated database snapshot for tests and experiments. Fixtures may be inserted into a temporary branch; teardown may be deleting that branch rather than truncating a shared database.

**ORNA-TEST-007** Test tooling SHOULD support temporary branch/worktree isolation without requiring a separately administered test server.

**ORNA-TEST-008** Branch-based tests MUST NOT imply that mocks or conventional unit tests are forbidden; the branch model is an integration-state primitive, not a ban on other testing methods.

## Mandatory behavioral scenarios

At minimum:

- CWD includes a local row while HEAD does not;
- automatic IDs never reuse a deleted/reset value;
- nested function writes roll back with the activation;
- an external HTTP effect is not rolled back;
- checkpoint and row writes commit together;
- poison failure pauses without infinite durable rows;
- explicit skip preserves/references payload and advances checkpoint;
- publication crashes before and after ref update recover correctly;
- automatic data commit excludes staged human changes;
- semantic merge handles compatible and conflicting schema/row edits;
- historical `sys.Run.as_of(HEAD)` is valid;
- page delta falls back to subtree replacement for an unkeyed value;
- display override does not change codec output;
- partial clone fetches only required segment blobs.


## Evidence levels

A fixture is a proposed test input and oracle. A syntax probe checks only its declared parsing expectation. A semantic reference model tests a selected algorithm outside an Orna implementation. A full implementation test additionally resolves, checks effects/types, executes and verifies results in the specified environment. These evidence levels MUST NOT be conflated.

The machine-readable evidence register maps each executed check to its actual subject, command and result. A requirement-family label is organisational metadata, not proof that every requirement in that family was exercised. Unexecuted implementation and interoperability obligations remain explicitly unexecuted.

**ORNA-EVIDENCE-001** A release report MUST distinguish authored conformance cases from executed implementation evidence and MUST NOT infer a pass from a filename, requirement association, file count or manifest validity.

**ORNA-EVIDENCE-002** A complete example project MUST include every required module/schema or pin an available dependency with an exact interface. Placeholder connector attachment points do not make an example self-contained.


## Scope of conformance

**ORNA-CLOSURE-002** Benchmark, cross-platform, interoperability, security and fault obligations determine an implementation's evidence for its claimed profile. They MUST NOT be used as permission to choose different observable semantics.

**ORNA-CLOSURE-003** Features outside the declared language/profile are absent, not implicitly implementation-defined. A source program relying on such a feature MUST receive an unsupported-feature or resolution diagnostic rather than silently acquiring vendor-specific behaviour under the same portable coordinate.

Conformance requires agreement with all applicable normative provisions. Report conflicting provisions as specification defects.
