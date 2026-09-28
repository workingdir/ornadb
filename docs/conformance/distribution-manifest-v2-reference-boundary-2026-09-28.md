# Distribution Manifest Format 2: Orna 1.0.0 Boundary

Beads: `ornadb-1787960947221-16-bef83ee9` (GitHub #100)
Audited base: `origin/main` at `9b4aa4f7d6f031bc7875094d7a0546a7619adba5`
Reference: `/home/pbox/dev/ornadb/reference/Orna-1.0.0`

## Ruling

No packaging or verifier changes are made under the Orna 1.0.0 reference-bound
lane. The frozen reference has no installed distribution-manifest schema,
product or Debian version fields, or self-excluding payload inventory
requirement. Format 2 is specified by repository decision ADR 0047, but that
release-packaging contract is not an `ORNA-*` requirement in the frozen
reference. Applying it here as Orna 1.0.0 conformance would exceed the stated
reference boundary. Keep the current development package identity unchanged.

This is a gated no-delta ruling, not a claim that ADR 0047's release task is
implemented. To resume implementation, the release/package lane must accept
ADR 0047 as the independent authority for this slice or map the format-2
contract into the frozen reference. No format behavior is inferred here.

## Reference evidence

The frozen reference search for the requested distribution-manifest terms
returned no matches:

```text
$ rg -n -i --glob '!*.pdf' 'distribution[ -]manifest|installed manifest|payload inventory|debian version|product_version|debian_version' /home/pbox/dev/ornadb/reference/Orna-1.0.0
[no matches]
exit: 1
```

The adjacent normative clauses are narrower:

- ORNA-FORMAT-001, `source/29-formats.md:13`, defines canonical OVB-1 value
  encoding and strict decoding; it does not define an installed package
  manifest.
- ORNA-EXT-001, `source/26-security.md:15`, says Orna must not require a
  separate handwritten permissions/package manifest merely to restate a
  component's WIT imports and exports. It does not specify distribution
  payload inventory or installed-manifest verification.

The repository's accepted `docs/decisions/0047-first-one-zero-release.md`
does define an installed distribution-manifest format 2 (lines 539–570),
including product and Debian versions and the manifest's self-excluding
payload inventory. That is explicit release-mechanism evidence, but it does
not add an `ORNA-*` requirement to the frozen language reference.

## Current implementation evidence

At the audited base, `packaging/linux/package.py` contains one generated
`usr/share/orna/distribution-manifest.toml` and a separate embedded-engine
manifest. Its `parse_distribution_manifest` accepts strict format 1 only
(lines 250–286); both archive verification and installed-payload replacement
call this parser (lines 511–512 and 736–738). The current package test verifies
that installed manifest and installation behavior in
`packaging/linux/test_package.py:129–153`.

Thus the repository has an existing format-1 distribution-manifest surface,
but no frozen-reference criterion for changing it to format 2. The same
generated manifest is checked in its archive and installed-payload paths; the
reference does not require two independent manifests.

## Validation

This documentation-only ruling changes no packaging behavior. Cargo tests were
not run. The read-only reference search above exited 1 because it found no
matching text. No release identity or generated package artifact was changed.
