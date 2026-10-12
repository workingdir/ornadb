# Release manifest and provenance audit

This audit separates the frozen specification-publication bundle from the current Linux executable package. It records the verified contents and validators without treating a reference packaging convention as an unstated Orna language requirement.

## Reference obligations and their scope

The frozen specification's `ORNA-PUB-*` requirements describe runtime/database publication: object durability, retention of pending batches, recovery after ref advancement, snapshot visibility, compare-and-set, index reconciliation, and conflict handling. In particular, `ORNA-PUB-005..009` are at `Orna-1.0.0.md:3768-3776`, `ORNA-PUB-010..012` at `:3746-3750`, and `ORNA-PUB-013..017` at `:3778-3801`. They do not specify the contents of a *specification release ZIP*.

I searched frozen `source/`, `tests/`, and `api/` for `release.json`, `file-manifest.json`, `SHA256SUMS`, and `provenance/inputs.json`; the search returned no matches. No ORNA-* requirement was found that mandates these filenames, a release signing policy, or one universal distribution archive layout. The classifications below distinguish frozen-publisher behavior and integrity checks from normative Orna runtime behavior.

| Declared contract or evidence | Classification | Evidence and boundary |
| --- | --- | --- |
| `release.json.version` and `normative_payload_sha256` | Implemented and verifiable | `orna-conformance-v1` and `orna-traceability-v1` require version `1.0.0`, require 46 normative member digests, and hash the referenced members. The new test calls `Corpus::load` and confirms the same map. This verifies the normative source payload inventory, not the rest of the release metadata. |
| `release.json.publications` | Verifiable; reference-publisher metadata | Five publication records name the HTML, Markdown, and PDF artifacts with SHA-256 values. `tools/package_release.py` verifies its PDF review hashes; the new test verifies all five named file digests and confirms they occur in `file-manifest.json`. Current Rust release consumers do not read this array. |
| `file-manifest.json` | Implemented by the frozen publisher and verifiable | Frozen `tools/package_release.py` inventories every distributed regular file except `file-manifest.json` and `SHA256SUMS`. The new test confirms all 415 listed paths, sizes, and hashes against the complete frozen tree. No Rust product consumer for this file was found. |
| `SHA256SUMS` | Implemented by the frozen publisher and verifiable | The publisher includes every distributed file except `SHA256SUMS` itself. The new test verifies all 416 entries and their digest/path coverage, including `file-manifest.json`. No Rust product consumer was found. |
| `provenance/inputs.json` and its three source archives | Integrity verifiable; provenance meaning is reference-only | The release record points to `provenance/inputs.json`. The new test verifies its three archive paths, byte lengths, and SHA-256 values and checks that they are in the complete file manifest. Hashes establish byte integrity; they do not authenticate authorship or assert an external trust policy. |
| Other `provenance/` editorial notes, patches, findings, and requirement-lineage records | Reference-only for editorial meaning; byte integrity covered by the file inventory | These files are distributed and hashed, but neither the Rust consumers nor an ORNA-* requirement validates the historical/editorial assertions they contain. |
| Release status, date, requirement/model counts, review dispositions, limitations, and packaging text in `release.json` | Reference-only metadata | The current Rust consumers use the version and normative digest map. They do not certify the release record's counts, review claims, syntax/model outcomes, or packaging statement. |
| Archive identity/signature or provenance trust policy | Reference-undefined; no delta | No signature, signer identity, certificate chain, or trusted provenance policy is declared by the searched normative source. Do not infer one from the presence of SHA-256 fields. |

## Exact frozen bundle snapshot for a release-tag decision

For a tag reproducing this frozen specification publication with its `tools/package_release.py`, the actual bundle input set is the 417-file tree under the archive root `Orna-1.0.0/`. The packager includes regular files recursively, excluding `__pycache__` directories and `.pyc` files; it refuses symbolic links, raw font files, parent traversal, backslashes, and newline-containing paths. Its generated records have these exact boundaries:

* `release.json` carries publication identity/status, review and model counts, five publication digest records, page counts, a 46-member `normative_payload_sha256` map, references to `provenance/inputs.json`, findings and review evidence, limitations, and the deterministic-packaging statement.
* `file-manifest.json` contains 415 files: every distributed file other than itself and `SHA256SUMS`, with relative path, byte count, and SHA-256.
* `SHA256SUMS` contains 416 files: every distributed file other than itself, including `file-manifest.json`.
* `provenance/` is included in full. `provenance/inputs.json` names `orna-specification-v1.0.0.zip`, `Orna-1.0.0-definitive-review.zip`, and `orna-spec-implementation-draft-v0.8.zip` with byte lengths and SHA-256 digests.
* The published tree also contains the source/API/grammar/profile payloads, `Orna-1.0.0.md`, both HTML publications, both PDFs, README, evidence/review outputs, tests and vectors, and the frozen `tools/` scripts. The manifest is the authoritative exact path list for this snapshot.

The frozen publisher sets a fixed ZIP member timestamp and mode, rejects duplicate or unsafe archive members, validates ZIP CRCs and internal checksums after extraction, and compares two builds for byte identity. Its default mode additionally reruns selected authoring checks after extraction and confirms normative payloads remain unchanged. This audit's direct publisher invocation used `--skip-execution`, so it verified packaging, extraction, reproducibility, and checksums but did not rerun those selected checks.

This exact snapshot is a release-tag decision input for the *frozen reference publication*. It is not a claim that ORNA-PUB requires every Orna product release to contain this specification corpus.

## Current product packaging boundary

`packaging/linux/package.py` is a separate executable-installation packager. Its archive is exactly `usr/bin/orna`, `usr/share/orna/distribution-manifest.toml`, and `usr/share/orna/embedded-engine-manifest.json`. Its manifest binds the build tools, Cargo lock, builder image, embedded engine, and executable. `packaging/linux/test_package.py` checks deterministic archives, exact member sets, input binding, tampering, modes, and safe installation. This implementation does not consume the frozen specification's `release.json`, `file-manifest.json`, `SHA256SUMS`, or editorial provenance. No cross-contract between these two archive types is defined; they are not interchangeable.

## Captured proof

The new `crates/orna-conformance-v1/tests/release_manifest_audit.rs` checks the actual frozen reference files. It validates the Rust conformance consumer's 46-member normative release inventory, the five publication hashes, complete file-manifest and SHA256SUMS sets, package exclusion rules, and all three provenance input archives.

Frozen publisher command (run against a temporary copy so the reference tree stayed unchanged):

```text
$ python3 tools/package_release.py --skip-execution --output /tmp/ornadb-96zz-Orna-1.0.0.zip
{
  "zip_members": 417,
  "crc_passed": true,
  "no_duplicate_or_unsafe_members": true,
  "internal_hashes_verified": 416,
  "fresh_extraction_selected_checks": {"status": "not requested"},
  "bytes": 38964081,
  "sha256": "3f20439c428b5809fa26bf71a7b5d40c9ec6b48a8566cbe9e426a25437c3de75",
  "deterministic_repeated_build": true
}
PACKAGE_EXIT_CODE=0
```

Rust test output:

```text
$ cargo test --locked --offline -p orna-conformance-v1 --test release_manifest_audit
running 2 tests
test release_file_walk_matches_the_reference_packager_exclusion_rules ... ok
test frozen_release_inventory_and_provenance_bind_the_complete_bundle ... ok

test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 7.52s
CARGO_EXIT_CODE=0
```
