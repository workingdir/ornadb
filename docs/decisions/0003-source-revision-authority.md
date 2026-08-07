# ADR 0003: Active Source Revision Is Authoritative

**Status:** Accepted

## Decision

The active database source revision is the canonical runtime source of truth.
Project files are authoring inputs, not runtime authority.

Source apply includes an expected base revision. An apply with a stale base
revision fails. It must not overwrite a newer active revision.

The system activates source, lossless syntax, semantic catalogue changes,
physical PostgreSQL changes, and generated artefacts in one transaction. It
activates all of them or none of them.

After restart, OrnaDB reconstructs its active state from durable catalogue
data. It does not require undeployed local source files.

## Precedence

This accepted amendment supersedes the conflicting or incomplete parts of
these sources:

* `spec/docs/25-source-compiler-ir.md` describes files as human-facing source
  and transactional apply but does not make the active database revision
  canonical or require an expected base revision.
* `spec/api/compiler.md` defines apply without an expected base revision.
* `spec/docs/34-hot-reload.md` describes candidate revision activation without
  the required stale-base rejection.
* `spec/docs/36-storage-transactions.md` requires atomic program apply but
  does not require the complete durable recovery rule in this record.
* `spec/docs/06-bootstrapping-recovery.md` describes recovery commands but
  does not establish project files as non-authoritative runtime inputs.

For this subject, this record has precedence over those sources and their
derived examples.
