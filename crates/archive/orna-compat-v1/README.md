# orna-compat-v1

This crate is the frozen 1.0.0 compatibility reference model. It records the
identifiers, catalogue and revision hashes, system-function registry, security
surface and value models that OrnaDB 1.0.0 wrote and read, so the 1.1.0
implementation can be checked against what the old format meant.

It is not a production component. Nothing in the shipped binary may depend on
it (`docs/design/layers.toml` gives it layer 15 and the `archive` role, and R2
rejects it from the shipped closure). Its only consumer is the conformance
harness's compatibility and traceability tests.

The Orna 1.1.0 publication remains the implementation authority. Its
compatibility archives are old-reader inputs, not competing goals, and the
legacy rules frozen here must not impose format or path restrictions on the
1.1.0 store (`reference/Orna-1.1.0/implementation/ORCHESTRATOR.md` and
`reference/Orna-1.1.0/Orna-1.1.0.md`, "Persisted rows and path projections").

The package is `publish = false` and is intentionally not named after a layer:
it is an archive of a version, not a core.
