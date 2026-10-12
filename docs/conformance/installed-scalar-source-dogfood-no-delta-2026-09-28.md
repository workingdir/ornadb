# Installed scalar source dogfood: no-delta ruling

Issue: `ornadb-1787968130436-72-fe4c9cd2` (GitHub #171)

## Ruling

No implementation delta is authorized by the frozen OrnaDB 1.0.0 reference
for the requested installed-product sequence: source check, source apply,
execute grant, then `orna invoke` of a parameterized scalar SERVER function.
The reference specifies typed language invocation behavior, but not this CLI,
grant, or installed-product proof contract. This record does not promote the
repository's work decisions into normative Orna requirements or invent a
replacement demo workflow.

## Verified reference boundary

- `source/18-cli.md:16-29` lists the Orna-specific CLI commands and says there
  are no required `orna grants` or extension-permission command hierarchies.
  **ORNA-CLI-002** at line 31 says runtime/checkpoint/failure details and
  uncommon recovery operations SHOULD use typed `sys` values in the REPL or
  optional devtools pages.
- **ORNA-TRUST-001**, `source/26-security.md:5`, puts local commands inside
  the invoking OS user's trust boundary and says Orna v1 defines no grants,
  principals, or related role/policy system.
- **ORNA-SYS-077**, `source/15-system.md:277`, constrains `sys.invoke` from
  evaluating arbitrary source text or bypassing parsing, resolution,
  typechecking, visibility, effects, or transactions. It does not specify an
  `orna invoke` CLI workflow.

The bounded reference searches were run against
`/home/pbox/dev/ornadb/reference/Orna-1.0.0`:

```text
$ rg -n -i 'CREATE SERVER FUNCTION|parameterized scalar SERVER|parameterised scalar SERVER|scalar SERVER demo|installed-product proof' source
[no output]
exit code: 1

$ rg -n -i 'source check|source apply|security grant-execute' crates/orna-cli-v1/src crates/orna-cli-v1/tests
[no output]
exit code: 1
```

The first search found no normative source example or installed-proof contract
for the requested form. The second is implementation evidence: these commands
are absent from the current `orna-cli-v1` source and tests. The repository
work ADRs 0018, 0038 and 0056 describe project decisions, not additional
ORNA-* requirements in the frozen reference.

## Current implementation boundary

The closest existing dogfood is
`crates/orna-application-v1/tests/accepted_server_function_dogfood.rs:8-20`.
It directly admits and evaluates `fixtures/server-function-dogfood.orna`
through `ApplicationAuthority`. That fixture defines a zero-argument
`read(): Int`; it does not invoke an installed product, apply a source
revision, or exercise an execute-grant workflow.

The current CLI parser at `crates/orna-cli-v1/src/cli_args.rs:248-256`
describes `invoke` as requiring a reachable zero-argument pure function. The
captured executable help is:

```text
$ cargo run --locked --offline -p orna-cli-v1 -- --help
orna-cli-v1 [OPTIONS] [COMMAND]
Orna commands:
  repl [EXPRESSION]
  run [QUALIFIED_FUNCTION]
  check
  invoke TARGET
  explain CODE
Repository and maintenance commands:
  init [DIRECTORY]
  status [--porcelain|--short|--format human|short|json]
  --format human|short|json status
  fetch [REMOTE] [BRANCH]
  diff [GIT_DIFF_ARGS...]
Options: --color auto|always|never, --db ENDPOINT
exit code: 0
```

Tests were not run: this evidence-only ruling adds no behavior or test. No
Orna source was added or embedded in a Rust test.
