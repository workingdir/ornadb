# OrnaDB

OrnaDB is a database platform for typed applications, with SERVER functions beside the data and CLIENT functions in the application.

## Features

- One function model covers queries, mutations, user interfaces, presenters, and integrations.
- Stable identities and revisions apply across code, data, and dependencies.
- PostgreSQL-backed storage provides transactions, constraints, and server-side functions; a local SQLite backend provides a file-backed development path.
- Client runtimes provide application interfaces, state, resources, actions, and local presentation.
- Structured invocation traces show execution details and security decisions.
- Source checking, semantic diffs, and editor integrations support local development.

## Build

Requirements:

- Rust 1.95 or later
- Cargo
- `just` for examples and repository checks

Build from a source checkout:

```sh
cargo build --locked --workspace
cargo run --locked -p orna-server -- --help
```

## Try it

Run the included examples without PostgreSQL or a native runtime:

```sh
just demo-suite
```

Exercise the complete file-backed SQLite path without PostgreSQL:

```sh
just sqlite-smoke
```

For a local SQLite database, build the CLI and pass its filesystem path:

```sh
cargo build --locked -p orna-server
target/debug/orna --db ./app.sqlite source check ./app.orna
target/debug/orna --db ./app.sqlite source apply ./app.orna
target/debug/orna --db ./app.sqlite source diff ./app.orna
```

For a complete local PostgreSQL server and invocation example:

```sh
just local-cli-demo
```

## Command line

```sh
target/debug/orna --help
target/debug/orna --version
```

The no-argument form targets the local function-backed session, and `orna repl`
is its explicit spelling. The parser accepts `orna invoke <function>` for one
stored function call:

```sh
orna
orna repl
orna invoke std.invoke.echo --arg p_value=hello
orna --db orna+unix:///run/orna/orna.sock invoke tasks.overdue
orna --db orna://db.example.test/work invoke tasks.overdue
orna --daemon
```

The examples above cover parser-supported command shapes. They do not prove
that a database host is available or that the session completes successfully.
The `--db` option selects a managed local database, an explicit Orna Unix
socket, or a remote Orna URI. Remote invocation is parsed by the CLI but is
not available until the authenticated TLS session contract is accepted.

Use `orna inspect`, `orna source check`, and `orna source apply` for normal
inspection and source workflows. Server administration, security management,
runtime metadata, and `raw-call` are explicit operational or recovery paths.
Use command-specific help for their options.

## Desktop runtime

OrnaDB supports terminal output and an optional Qt desktop runtime. Run the demos with:

```sh
just runtime-tty-demo
just studio-qt-demo
```

The Qt runtime is built separately from the OrnaDB server. See the maintainer documentation for its prerequisites.

## Development

Run common checks from the repository root:

```sh
just test
just editor-tooling-check
just demo-suite
```

## Documentation

See the [maintainer and operator documentation](docs/maintainer-runbook.md) for environment setup, runtime development, and operations. See [editor tooling](docs/editor-tooling.md) for editor-specific checks.

## Project layout

The repository contains the Rust workspace, standard library declarations, runtimes, editor integrations, and repository tooling.

## License

OrnaDB is licensed under the [Apache License 2.0](LICENSE).
