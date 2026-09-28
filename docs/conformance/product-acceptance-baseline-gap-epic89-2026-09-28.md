# Product-acceptance baseline and Debian-path deferral

Release-epic progress increment 8 for [OrnaDB v1.0.0 product release (#89)](https://github.com/workingdir/ornadb/issues/89). This is one scoped evidence/deferral record for the absent product-acceptance baseline and Debian release paths. It does not create a baseline or propose release steps.

## Frozen-reference boundary

The frozen reference publication is Orna 1.0.0. Its `release.json` publication entry for `Orna-1.0.0.md` and a fresh SHA-256 both report:

```text
d12cf5d86b9337ccbe45f257bcb8c25bc769e0505500bdc68e21a1b67d728d7d
```

The relevant normative clauses describe conformance claims and evidence reporting:

- **ORNA-CONF-001** (`source/01-scope.md:50`) requires any conformance claim to identify its classes, implementation version, and exact publication digest.
- **ORNA-CONF-002** (`source/01-scope.md:52`) requires a claim to distinguish supported optional profiles from mandatory facilities and list the implementation tests actually executed.
- **ORNA-CONF-003** (`source/01-scope.md:54`) requires applicable fixtures and behavioural tests to pass before claiming the corresponding class.
- **ORNA-TEST-004** (`source/32-conformance.md:23`) distinguishes specified tests, tests present in an implementation, and tests passed; document/index checks cannot claim Orna source executed without an implementation run.
- **ORNA-EVIDENCE-001** (`source/32-conformance.md:69`) requires release reports to distinguish authored cases from executed implementation evidence and forbids inferring a pass from filenames, mappings, counts, or manifest validity.

The frozen reference does not define a product-acceptance-baseline path or Debian release-path contract. The captured targeted search was:

```text
rg -n -i '(product[- ]acceptance|acceptance baseline|docs/releases/1\.0-product-acceptance\.md|packaging/debian/(changelog|rules)|debian-release\.yml|\.deb|apt repository)' source tests examples
[no matches]
FROZEN_REFERENCE_SEARCH_EXIT_CODE=1
```

Exit 1 is ripgrep's no-match result for this query. This absence means those release artifacts are not specified by the frozen Orna language/runtime reference; it does not mean that such release obligations are optional under repository governance.

## Repository evidence and disposition

The repository release decision `docs/decisions/0047-first-one-zero-release.md` separately requires an accepted product baseline at `docs/releases/1.0-product-acceptance.md` before the protected 1.0.0-1 declaration/publication. It also names Debian release authorities and paths. These are repository release-governance decisions, not ORNA-* clauses in the frozen reference.

At audited `origin/main` revision `e5fd4f049a65f8e513060ede58716f6904f2a820`, the path check returned:

```text
ABSENT docs/releases/1.0-product-acceptance.md
ABSENT packaging/debian/changelog
ABSENT packaging/debian/rules
ABSENT .github/workflows/debian-release.yml
DEBIAN_DIRECTORY_FILES=0
```

The baseline and named Debian release files therefore remain absent at this revision. This record defers product acceptance and Debian release-path readiness; it does not infer product support, weaken the repository decision, or prescribe an implementation sequence. The release epic remains open.

Cargo tests were not run: this increment adds only an evidence/deferral document and makes no implementation or Orna-source change.
