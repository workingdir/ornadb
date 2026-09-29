# Editorial and rendered evidence audit

Issue: `ornadb-duz3` (Editorial checks and rendered evidence audit).
Reference: `/home/pbox/dev/ornadb/reference/Orna-1.0.0`.
Implementation base: `d9671688` (`origin/main` at worktree creation).

## Classification rule

`implemented-as-reference-check` means the frozen publication evidence records a completed editorial or rendering-tool check. `implementation-relevant` is limited to a behavior that OrnaDB code can itself validate against a normative ORNA requirement. `absent` means no such implementation check is present in these evidence records. A publication tool checking its own HTML, search script, or PDF is not thereby an OrnaDB runtime contract.

Relevant normative boundaries are **ORNA-CLI-002** (`source/18-cli.md:31`), which recommends typed `sys` values for runtime/checkpoint/failure details and optional `std.devtools` pages; **ORNA-UX-001..004** (`source/18-cli.md:106-112`), which define Orna-concept wording and actionable user-facing diagnostics; **ORNA-PRES-010** (`source/13-presentation.md:117`), which requires an Inspect-compatible fallback for unknown rich nodes; and **ORNA-SYS-046/047** (`source/15-system.md:156,168`), which keep diagnostic structure available independently of rendering and forbid rendering from mutating diagnostic identity. These clauses do not specify the reference site's editorial phrase set, page layout, PDF geometry, link counts, or search hit counts.

## Declared checks

| Evidence record and declared check | Recorded result | Classification | Implementation disposition |
|---|---|---|---|
| `editorial-checks.json`: publication input identity (`version`, `change_kind`, archive name and SHA-256) | version `1.0.0`, `editorial`, `Orna-1.0.0.zip`, SHA-256 `28f29f34…9e664c10` | implemented-as-reference-check | Reference provenance only. **ORNA-CONF-001** (`source/01-scope.md:50`) applies to implementation conformance claims; this editorial archive hash is not such a claim. |
| Changed authoring-file scope and protected-file inventory | 27 authoring paths changed; 180 protected example/grammar/vector paths declared unchanged | implemented-as-reference-check | Reference source-scope check; it does not exercise runtime behavior. |
| API declarations and purpose descriptions | declarations unchanged; one purpose description edited | implemented-as-reference-check | Reference API-document editorial check. No OrnaDB code check is stated. |
| Fenced listings | `fenced_listings_unchanged: true` | implemented-as-reference-check | Reference-source preservation check. |
| Numbered requirement audit | 870 requirements; five listed wording-only changes (`ORNA-EXT-004`, `ORNA-EXT-005`, `ORNA-CLONE-001`, `ORNA-SERVE-003`, `ORNA-STORAGE-010`); `ORNA-MIGRATE-006` publication-maintenance note moved to provenance; other numbered statements unchanged | implemented-as-reference-check | Normative-text editorial audit, not a behavioral implementation test. The identifiers are reported as data in this record; the record does not claim implementation coverage. |
| Phrase checks over `index.html`, `sys-reference.html`, `Orna-1.0.0.md`, `README.md`, `examples/reference/README.md`, and both PDFs | seven entries; each `identified_phrases_remaining` array is empty | implemented-as-reference-check | Reference wording check. It does not define the phrase list as an Orna user-facing requirement. **ORNA-UX-001/002** (`source/18-cli.md:106,108`) separately govern generated user-facing text, but this evidence does not test actual CLI, REPL, diagnostic, or frontend output. |
| Diagnostic-table shape | 23 rows including header, 3 columns, pipeline tokens preserved | implemented-as-reference-check | Reference table-integrity check. **ORNA-UX-004** (`source/18-cli.md:112`) is an implementation-relevant rule for diagnostics, but this table check does not inspect emitted diagnostics. |
| `editorial-checks.json` visual inspection and `editorial-rendered/` PNGs | seven page/hash records: `Orna-1.0.0.pdf` pages 1, 5, 155, 175, 183, 186; system-reference PDF page 1 | implemented-as-reference-check | Selected reference-publication screenshots only; not a runtime renderer test. **ORNA-PRES-010** (`source/13-presentation.md:117`) is implementation-relevant, but these PDF images do not exercise unknown-node fallback. |
| `pdf-geometry.json`: page count, out-of-page text, replacement/NUL glyphs, A4 size | main PDF 271 pages; system reference 94; all three error arrays empty for both | implemented-as-reference-check | Reference PDF geometry check only. No corresponding PDF geometry behavior is specified for OrnaDB runtime output. |
| `pdf-link-portability.json`: page count, relative/absolute local links, digest | main PDF: 271 pages, 12 relative and 0 absolute local links; system reference: 94 pages, 53 relative and 0 absolute local links | implemented-as-reference-check | Reference packaging/link-portability check only. No corresponding implementation contract is declared. |
| `navigation-model.json`: HTML anchor targets and inline search-script behavior | Node `v22.16.0`; 1,849 entries; all targets exist; five queries below pass | implemented-as-reference-check | Reference-site navigation/search check. Its declared scope expressly says minimal DOM, not browser or accessibility testing. Search hit counts are reference-site data, not OrnaDB behavior. |
| Runtime availability counterpart for the navigation query `sys.rt.info` | Existing real-fixture check below passes parse, resolve, and typecheck | implementation-relevant | The query token is also a typed runtime API, and **ORNA-CLI-002** (`source/18-cli.md:31`) makes typed `sys` runtime details implementation-relevant. The new test checks only symbol use; it makes no claim about the site's two search hits. |
| Runtime CLI/REPL wording, actual diagnostic rendering, rich-node fallback, browser navigation/accessibility | not represented in these evidence records | absent | These require separate implementation tests if accepted as work. The reference editorial/PDF records do not establish these results; see **ORNA-UX-001..004**, **ORNA-PRES-010**, and **ORNA-SYS-046/047** at the locations above. |

### Navigation query rows

| Search query | Hits | Classification | Reason |
|---|---:|---|---|
| `sys.rt.info` | 2 | implemented-as-reference-check; typed API use separately implementation-relevant | The two-hit assertion belongs to the reference search index. Typed use is separately checked by the new real-fixture test under ORNA-CLI-002. |
| `ORNA-CONCUR-001` | 1 | implemented-as-reference-check | Confirms a normative identifier is searchable in the reference site; it does not execute concurrency behavior. |
| `Branching with pending changes` | 1 | implemented-as-reference-check | Confirms a reference heading is searchable; it does not test branch semantics. |
| empty query | 0 | implemented-as-reference-check | Reference search UI behavior only. |
| `<img onerror=bad()>` | 0 | implemented-as-reference-check | Reference search query behavior only; not a general browser security or accessibility test. |

The visual-inspection records point to these frozen image artifacts and SHA-256 values:

| PDF / page | Render | SHA-256 |
|---|---|---|
| Main / 1 | `editorial-rendered/book-001.png` | `95ae0b99d47a9b7a035805c99c677bc59f2ff90ec01bd740293fa6f6acce6073` |
| Main / 5 | `editorial-rendered/book-005.png` | `131154287141bdac731098fca67fd76bdaabcc9ef572a01e8a8fea4575896986` |
| Main / 155 | `editorial-rendered/book-155.png` | `f10ea05262a308955d42ba97c4a5d0db3b0afd64d625484909c98aadbb70400c` |
| Main / 175 | `editorial-rendered/book-175.png` | `2c301f939915aabd238c8e2065d2dcd8f59a1f64e607cf2a81dc40e6713b1711` |
| Main / 183 | `editorial-rendered/book-183-fixed.png` | `472a0cacce3e0c945bbdfe093e0102aa9ae16694f1f55070799d32b51392fe82` |
| Main / 186 | `editorial-rendered/book-186.png` | `8a87ee11fc5ca2263fcea0be16f21ceab18ac8ff66665f4e00a0003d2cb49ede` |
| System reference / 1 | `editorial-rendered/sys-001.png` | `b09cf81a2ccdd19e2103ffdc5c75d7fd2a8511d9169e560e5f8bd684b9dfd9ba` |

## Implementation test and captured proof

The implementation-relevant slice is `sys.rt.info` symbol use. The test uses the checked-in real fixture `crates/orna-conformance-v1/tests/fixtures/sys-rt-info.orna` through `include_str!`; it does not recreate the reference search-count behavior.

```text
ORNA_REFERENCE_DIR=/home/pbox/dev/ornadb/reference/Orna-1.0.0 cargo test --locked -q -p orna-conformance-v1 --test editorial_navigation_runtime_crosscheck -- --nocapture

running 1 test
.
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
CAPTURED_EXIT_CODE=0
```

An additional broader existing API-surface test was attempted but is not a passing proof for this slice: `portable_sys_api_has_exact_declared_counts_and_surface` failed because `value_types` was 35 while that test expects 34 (exit code 101). The narrower `sys_runtime_info_schema_matches_the_published_compatibility_contract` passed (1 passed; exit code 0). Captured transcripts: `/tmp/editorial-evidence-duz3-sys-api-test.log` and `/tmp/editorial-evidence-duz3-sys-runtime-info-test.log`.

No reference evidence, existing test, production source, formatter, or gate was changed. The only new test checks implementation-relevant typed-symbol use; all other declared checks remain reference-only.
