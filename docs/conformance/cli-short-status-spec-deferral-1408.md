# CLI short-status specification gap and deferral

Issue: `ornadb-1787968123319-16-24513f57.5` (GitHub #1408)
Parent: `ornadb-1787968123319-16-24513f57` (GitHub #89)
Reference: `/home/pbox/dev/ornadb/reference/Orna-1.0.0`

## Question and decision

Does the frozen Orna 1.0.0 reference define both (1) a stable row vocabulary
for runtime summaries in `orna status --short`, and (2) behavior when the
worktree's `.git/orna/state.db` is absent?

**No.** The reference requests stable codes, but does not specify their values,
row grammar, or presentation contract. It establishes the authority of
`state.db` for unpublished runtime changes, but does not say whether a missing
database means an empty runtime summary, an error, or another result. None of
those behaviors can be selected from the reference without inventing contract.

This records a deferral of the short-status runtime summary. It resolves the
blocked decision question for #1408; it does not claim that `ORNA-CLI-004` is
implemented and does not add a product behavior.

## Verified reference boundaries

The frozen source contains these relevant clauses:

* **ORNA-CLI-004**, `source/18-cli.md:58`: `orna status --short` SHOULD preserve
  Git's compact style while adding stable codes for runtime CWD summaries. It
  names the desired shape at a high level but gives no code vocabulary or
  stable short-row grammar.
* **ORNA-CLI-003**, `source/18-cli.md:56`: `orna status --porcelain` MUST provide
  stable machine-readable output. This requirement is expressly for
  `--porcelain`; it does not define `--short` rows.
* **ORNA-LOCAL-002**, `source/19-repository.md:96`: `state.db` MUST be treated
  as authoritative for runtime changes that have not yet been published to
  Git. This identifies the data authority, not missing-store behavior.
* **ORNA-EMBED-002**, `source/19-repository.md:106`: local commands including
  `orna status` MUST be capable of opening embedded state in-process when no
  other local owner exists. This covers the owner boundary, not what to do
  when `state.db` does not exist.

## Captured search evidence

Searches were restricted to the frozen reference. The reference's combined
Markdown publication repeats the normative source text; it is shown here as a
second occurrence, not as a separate requirement.

```text
rg -n --glob '*.md' 'orna status --short|short[- ]status|stable codes for runtime CWD summaries' \
  /home/pbox/dev/ornadb/reference/Orna-1.0.0/source \
  /home/pbox/dev/ornadb/reference/Orna-1.0.0/Orna-1.0.0.md

/home/pbox/dev/ornadb/reference/Orna-1.0.0/Orna-1.0.0.md:3439:
  **ORNA-CLI-004** `orna status --short` SHOULD preserve Git's compact style while adding stable codes for runtime CWD summaries.
/home/pbox/dev/ornadb/reference/Orna-1.0.0/source/18-cli.md:58:
  **ORNA-CLI-004** `orna status --short` SHOULD preserve Git's compact style while adding stable codes for runtime CWD summaries.
exit code: 0

rg -n -i --glob '*.md' \
  'state\.db.{0,80}(absent|missing)|(?:absent|missing).{0,80}state\.db|state database.{0,40}(absent|missing)' \
  /home/pbox/dev/ornadb/reference/Orna-1.0.0/source \
  /home/pbox/dev/ornadb/reference/Orna-1.0.0/Orna-1.0.0.md
  [no matches]
exit code: 1

rg -n -i -C 2 \
  'ORNA-CLI-004|state\.db|runtime state|unpublished runtime|CWD' \
  /home/pbox/dev/ornadb/reference/Orna-1.0.0/source/18-cli.md \
  /home/pbox/dev/ornadb/reference/Orna-1.0.0/source/19-repository.md
  source/18-cli.md:56 ORNA-CLI-003 defines --porcelain stability.
  source/18-cli.md:58 ORNA-CLI-004 requests stable codes for short runtime CWD summaries.
  source/19-repository.md:90 state.db is described as authoritative uncommitted runtime CWD.
  source/19-repository.md:96 ORNA-LOCAL-002 makes state.db authoritative for unpublished runtime changes.
  [no text specifies absent-state status behavior]
exit code: 0
```

The full status example in `source/18-cli.md` is the ordinary `orna status`
view, not a `--short` code table. The frozen requirements index confirms the
same ORNA IDs and wording; its implementation-evidence records mark these
requirements planned/not executed and do not supply the missing CLI contract.

## Resolution and test status

Resolve the blocked #1408 scope as **deferred by the frozen specification**:
neither stable short-row vocabulary nor absent-`state.db` behavior is defined.
No row code, display grammar, fallback, store initialization, or error behavior
is chosen by this report. No CLI source or test files changed.

Tests were not run; this is a reference-only decision record.
