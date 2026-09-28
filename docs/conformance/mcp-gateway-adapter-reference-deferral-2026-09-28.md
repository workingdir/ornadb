# MCP gateway adapter: frozen-reference deferral

Progress for Beads `ornadb-1787784779022-36-4538ed8f` / GitHub #36, “Implement the MCP gateway adapter.” This is a bounded deferral record. The frozen OrnaDB 1.0.0 reference defines no MCP adapter behavior, so this change implements no transport or gateway behavior and leaves the issue available for a future approved contract.

## Reference search evidence

Search run against the complete frozen reference tree:

```text
$ rg -ni 'mcp|model context protocol|json-rpc|json rpc|gateway|reflective gateway' /home/pbox/dev/ornadb/reference/Orna-1.0.0
[no matches]
REFERENCE_SEARCH_EXIT=1
```

Exit 1 is ripgrep's no-match result; the command reported no read or parse error. The search covered all searchable text under the frozen root, including `source/`, `grammar/`, `tests/`, `examples/`, `api/` where present, `profiles/`, and evidence assets. `tests/requirements.json` and `tests/requirement-evidence.json` are included in that search and contain no MCP, JSON-RPC, or gateway clause/link.

## Adjacent normative boundaries

The checked clauses constrain existing Orna behavior but do not define an MCP adapter:

- **ORNA-PROTO-001**, `source/30-protocol.md:9`: validate a complete message, required fields, canonical forms, and limits before admitting an operation. The chapter's concrete protocol is `orna.present.v1` over WebSocket; it gives no MCP framing or translation rule.
- **ORNA-PROTO-002**, `source/30-protocol.md:23`: preserve request reservations and terminal outcomes during the specified Orna session-resumption lease. It does not map an MCP session or lifecycle onto that state.
- **ORNA-TRUST-001**, `source/26-security.md:5`: local commands trust the invoking OS user; v1 does not define principals, groups, grants, row ACLs, device roles, or an enterprise policy language.
- **ORNA-TRUST-002**, `source/26-security.md:7`: `orna serve` binds to loopback by default; remote deployment should use SSH, Tailscale, or a trusted authenticated reverse proxy/TLS boundary.
- **ORNA-TRUST-003**, `source/26-security.md:9`: public anonymous mutation and arbitrary remote REPL execution are outside the trusted v1 profile.

None specifies MCP framing/handshake, identity or authentication mapping, tool/resource method mapping, conversion/redaction rules, lifecycle, or adapter error mapping. Implementing the issue's proposed “same accepted canonical request boundary as JSON-RPC” would require inventing a JSON-RPC contract too; the frozen tree contains no JSON-RPC reference.

## Repository evidence and disposition

The repository occurrence of `McpGateway` is the `InvocationCallerKind` enum in `crates/orna-core/src/invocation.rs:575-579`. It classifies caller kind in an internal invocation request; it does not implement or authorize a gateway adapter. This documentation change does not modify that enum or the separate G1 gateway-lifecycle component.

Disposition: defer the MCP adapter until an accepted contract defines its wire framing, authentication and identity rules, canonical operation mapping, lifecycle, and errors. Do not infer these from the Orna live WebSocket protocol or the caller-kind enum. The existing `ORNA-TRUST-*` limits continue to apply; this note grants no anonymous or caller-selected authority.

Cargo tests: **not run**. This docs-only deferral adds no implementation or test behavior to execute. No Orna source or fixture is involved.
