# 20. Embedded ownership and local durability {#embedded}

## Purpose

Turso provides local transactional state for:

- pending high-rate rows;
- current checkpoints;
- retry/failure state;
- publication intents;
- runtime system observations that have not been published;
- local indexes that need transaction coupling.

**ORNA-TURSO-001** The runtime MUST use an embedded Turso/SQLite-compatible database, not require a Turso server, for the local CWD state profile.

**ORNA-TURSO-002** Runtime row writes and corresponding checkpoint updates MUST occur in one Turso transaction.

**ORNA-TURSO-003** The runtime MUST use durability settings appropriate to its claimed failure model and MUST document whether process crash, OS crash and power loss are covered.

## Local ownership

**ORNA-TURSO-004** At most one local process may own the writable embedded state when the selected Turso mode only supports single-process access.

**ORNA-TURSO-005** Other local commands SHOULD communicate with the temporary owner over a local IPC socket when one exists.

**ORNA-TURSO-006** When no owner exists, any command requiring access MAY become the owner for its lifetime.

The coordination process is created as needed; local operation does not require a persistent daemon.


## Git implementation boundary

**ORNA-IMPL-001** Implementations MAY use `gix`, libgit2 or the Git executable provided produced objects/refs and observable behavior conform to standard Git.
