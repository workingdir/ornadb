# Path vector materialisation inspection

Beads issue: `ornadb-h6ob`  
Scope: the three prose entries in the frozen `tests/path-vectors.json` `materialisation` field (lines 232–234). They are inspection prompts, not executable path-vector cases. The executable `fixture_path_vectors` test does not consume that field.

## Reference criteria and inspected evidence

| Materialisation entry | Frozen requirement | Implementation and focused proof | Evidence boundary |
| --- | --- | --- | --- |
| Decoded paths remain beneath the resolved table row directory. | ORNA-PATH-012 requires preventing absolute paths, traversal, symlink escape, and writes outside the table row directory (`source/07-tables.md:368`). | `LoosePath::from_encoded_key` and `LoosePath::for_key` decode and construct validated relative paths (`crates/orna-storage-v1/src/lib.rs:81`, `:89`). Repository materialisation joins beneath the worktree and validates parents (`crates/orna-repository-v1/src/lib.rs:5149`, `:5246`). `fixture_path_vectors` passed, but exercises value codec vectors, not filesystem row-directory materialisation. | Static code inspection plus the listed narrower test. No end-to-end row-directory containment case was run; this prose entry is not recorded as passed. |
| Symlinks and reparse points cannot redirect a write outside that directory. | ORNA-PATH-012 (`source/07-tables.md:368`). | `validate_managed_parent` inspects existing parents with symlink metadata (`crates/orna-repository-v1/src/lib.rs:5246`). The Unix integration test `managed_materialization_rejects_symlinked_parents` passed and checked that an outside row was not written (`crates/orna-repository-v1/tests/git_repository.rs:4942`). | The proof is Unix symlink-specific. No Windows reparse-point test was run, so this prose entry is not recorded as passed in full. |
| Case collision is detected before any destination is replaced. | ORNA-PATH-006 requires rejecting ASCII-case-fold collisions before overwrite (`source/07-tables.md:342`). | `validate_portable_paths` performs the portable collision check (`crates/orna-storage-v1/src/lib.rs:1282`); `portable_sibling_collisions_reject_the_entire_candidate` passed for both mutation orderings and unchanged candidate projection (`crates/orna-storage-v1/src/lib.rs:3584`). | This proves storage candidate preflight, not a filesystem row-destination replacement end-to-end. The prose entry is not recorded as passed in full. |

ORNA-PATH-007 defines composite-key component order and final-component `.orna` suffix (`source/07-tables.md:351`); the three materialisation prose entries do not add an executable ORNA-PATH-007 case.

## Conformance status

ORNA-TEST-004 distinguishes a specified test, an existing test, and a passed test; documentation must not claim Orna source executed when it did not (`source/32-conformance.md:23`). ORNA-TEST-010 allows an explicit inspection requirement in the test mapping and says that mapping is a plan, not proof of execution (`source/32-conformance.md:31`). Accordingly, this record distinguishes inspected implementation, narrower existing tests that passed, and the unexecuted full prose checks. The frozen requirement-evidence register remains unchanged: its PATH-006, PATH-007, and PATH-012 obligations remain planned/not executed, and no full implementation coverage is claimed.

## Captured focused test runs

`cargo test --locked --offline -p orna-value-v1 fixture_path_vectors`

```text
test tests::fixture_path_vectors ... ok
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 35 filtered out; finished in 0.00s
exit code: 0
```

`cargo test --locked --offline -p orna-storage-v1 portable_sibling_collisions_reject_the_entire_candidate`

```text
test tests::portable_sibling_collisions_reject_the_entire_candidate ... ok
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 134 filtered out; finished in 0.00s
exit code: 0
```

`cargo test --locked --offline -p orna-repository-v1 --test git_repository managed_materialization_rejects_symlinked_parents`

```text
test managed_materialization_rejects_symlinked_parents ... ok
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 130 filtered out; finished in 0.86s
exit code: 0
```

These focused passes are bounded evidence only. In particular, the runs do not establish Windows reparse-point behavior or full filesystem destination containment/collision-before-replacement for all three prose entries.
