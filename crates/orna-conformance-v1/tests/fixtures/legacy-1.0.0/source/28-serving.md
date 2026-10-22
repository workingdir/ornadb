# 28. Serving a database {#serving}

## Optional network host

`orna serve` provides network access to the current clone. Local commands use the [embedded runtime](#embedded) directly and do not require the server.

**ORNA-SERVE-001** `orna serve` provides Git transport, page/query endpoints and WebSocket presentation deltas for its clone.

**ORNA-SERVE-002** `orna serve` MUST NOT automatically invoke root `main()` or unrelated stream programs.

**ORNA-SERVE-003** A different clone running `orna serve` serves that clone's HEAD and CWD; CWD MUST remain local to that clone.

**ORNA-SERVE-004** The default listener MUST bind only to loopback unless the operator explicitly selects another address.

## Deployment trust {#serving-trusted-personal-deployment-model}

Host machines and local OS accounts are trusted. The [trust model](#security) defines the isolation boundary.

**ORNA-SERVE-005** Core Orna does not define principals, groups, grants, per-device roles or row-level permissions.

Remote exposure is expected to use existing boundaries such as Tailscale, SSH, host firewall rules or an authenticated reverse proxy. Git-over-SSH uses ordinary SSH keys. HTTPS deployments inherit authentication from the configured trusted proxy where desired.

**ORNA-SERVE-006** Binding beyond loopback MUST be explicit and MUST produce a clear status indication of the exposed interfaces.

Multi-user authorisation is outside this profile; local trusted operation MUST NOT depend on it.

## Git transport and browser frontend

**ORNA-SERVE-007** Git transport MUST remain usable when the optional Git/database web frontend is absent.

**ORNA-SERVE-008** The default frontend SHOULD be an ordinary Orna application, preferably `std.devtools`.

**ORNA-SERVE-009** When installed as `/`, the default frontend SHOULD lead with database tables and also expose files, commits, branches, functions, dependencies, storage and runtime state.

## Applications and renderers

An Orna application is a reachable collection of functions, pages, tables and assets. No `CREATE APPLICATION` grammar or application manifest is required merely to group code.

Presentation trees are renderer-neutral. Terminal, web and future native clients render the same value tree and fall back to Inspect-compatible nodes when needed.

