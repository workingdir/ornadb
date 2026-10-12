# Debian Release-Paths Gap: Evidence and Deferral

Epic: `ornadb-1787968123319-16-24513f57` (GitHub #89)
Audited base: `4ba34b2d4a1bb0a4a755ea9ddd7fb0c38278ac0a` (`origin/main`)
Reference: `/home/pbox/dev/ornadb/reference/Orna-1.0.0`

## Scoped finding

The readiness audit identifies three Debian production-release paths:
`packaging/debian/changelog`, `packaging/debian/rules`, and
`.github/workflows/debian-release.yml`. They are absent from the audited
`origin/main` tree. The current tree has Linux package scripts under
`packaging/linux/`; their presence does not supply the missing Debian paths or
establish the authenticated Debian package and repository release authority
specified by accepted decision `docs/decisions/0047-first-one-zero-release.md`
(see its decision section and implementation plan rows 886–891).

This is a scoped repository-state finding and deferral, not a release-readiness
approval or a claim that the release mechanism is implemented. This record
does not add package files, a build target, a release workflow, or a release
operation. A separate packaging slice must own those changes and their release
acceptance evidence.

## Reference boundary

No `ORNA-*` requirement is attributed to Debian packaging by this record. A
case-insensitive text search of the frozen reference for Debian, `dpkg`,
`.deb`, and Debian package/release terms returned no matches. Accordingly,
this increment documents the packaging deferral from the repository's
accepted release decision and captured tree evidence; it does not invent a
language or runtime requirement.

## Captured evidence

The following read-only checks were run against the frozen reference and
`origin/main` at the audited base above:

```text
$ rg -n -i --glob '!*.pdf' 'Debian|dpkg|\.deb|apt repository|Debian package|Debian release' /home/pbox/dev/ornadb/reference/Orna-1.0.0
[no matches]
exit: 1

$ for path in packaging/debian/changelog packaging/debian/rules .github/workflows/debian-release.yml; do
>   if git cat-file -e "origin/main:$path" 2>/dev/null; then printf 'present %s\n' "$path"; else printf 'absent %s\n' "$path"; fi
> done
absent packaging/debian/changelog
absent packaging/debian/rules
absent .github/workflows/debian-release.yml

$ git ls-tree -r --name-only origin/main -- packaging/linux
packaging/linux/package.py
packaging/linux/package.sh
packaging/linux/test.sh
packaging/linux/test_package.py
```

The source decision names the protected Debian mode and the sole changelog
authority; the readiness audit records these path absences. The check above
confirms the same gap at the newer audited base. It does not infer anything
about untracked local files or release artifacts outside this repository
tree.

No Rust, Orna, package, or workflow source was changed. Cargo tests were not
run for this documentation-only evidence/deferral increment.
