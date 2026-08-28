# OrnaDB

OrnaDB is a database platform for typed applications, with SERVER functions beside the data and CLIENT functions in the application.

## Features

- One function model covers queries, mutations, user interfaces, presenters, and integrations.
- Stable identities and revisions apply across code, data, and dependencies.
- PostgreSQL-backed storage provides transactions, constraints, and server-side functions.
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

For a complete local server and invocation example:

```sh
just local-cli-demo
```

## Command line

```sh
target/debug/orna --help
target/debug/orna --version
```

Use `orna` to check source, compare revisions, invoke functions, inspect calls, access state, describe runtimes, and manage local permissions. Command-specific help lists the available command options.

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

See the [maintainer and operator documentation](docs/maintainer-runbook.md) for environment setup, packaging, runtime development, and operations. See [editor tooling](docs/editor-tooling.md) for editor-specific checks.

## Project layout

The repository contains the Rust workspace, standard library declarations, runtimes, editor integrations, packaging, and repository tooling.

## License

OrnaDB is licensed under the [Apache License 2.0](LICENSE).
