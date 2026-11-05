# 26. Trust, isolation and extensions {#security}

Orna operates within a trusted environment. Host machines, OS accounts and repository access controls define the trust boundary.

**ORNA-TRUST-001** Local commands trust the invoking OS user. Orna v1 does not define principals, groups, grants, row ACLs, device roles or an enterprise policy language.

**ORNA-TRUST-002** `orna serve` binds to loopback by default. Remote deployment SHOULD use normal SSH, Tailscale, or a trusted authenticated reverse proxy/TLS boundary.

**ORNA-TRUST-003** Public anonymous mutation or arbitrary remote REPL execution is outside the trusted v1 profile.

## Extensions

Portable extensions use the WebAssembly Component Model/WIT where practical; system integrations may use a versioned out-of-process adapter; native extensions are explicitly trusted implementation-specific code.

**ORNA-EXT-001** Orna MUST NOT require a separate handwritten permissions/package manifest merely to restate a component's WIT imports and exports.

**ORNA-EXT-002** WIT imports/exports and the pinned Git object identify the portable interface and code. Installing/running the extension is the trust decision.

**ORNA-EXT-003** A Wasm extension MUST NOT receive raw pointers/handles to Turso or Git internals. Host calls use typed interfaces so a trap cannot corrupt repository/runtime memory.

**ORNA-EXT-004** Implementations SHOULD enforce coarse execution budgets and cancellation for portable components. These controls bound execution; they do not grant per-device permissions.

**ORNA-EXT-005** Native extensions run with the host process's trust and MUST be labeled as such. Orna does not sandbox native extensions.

## SOPS and repository privacy

SOPS-encrypted files may be committed normally. Decryption identities remain outside Git. Secret values are opaque/redacted by default across Inspect, Display, Present, codecs, diagnostics and traces.

Normal row deletion is versioned deletion, not erasure from Git history, remotes or backups. Explicit history rewrite/prune remains a separate destructive workflow.

## Public multi-user deployments

Multi-user authentication and authorisation are outside this profile. Trusted deployments do not require a separate multi-user authorisation service.

