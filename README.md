# OrnaDB

OrnaDB is a Git-backed database platform for typed applications. The current
implementation work targets the Orna 1.0.0 language, local repository, embedded
runtime, and command-line workflows.

## Current local CLI

The active local binary is `orna-cli-v1`. It works with a local Git-backed
project and keeps source checking, execution, and interactive evaluation on
the same Orna 1.0 path.

### Build

Use a Rust 1.95 (or newer) toolchain and Git:

```sh
cargo fetch --locked
cargo build --locked -p orna-cli-v1
```

### Initialize and check a project

Initialize a repository in the current directory or at an explicit path, then
check its reachable Orna source modules:

```sh
cargo run --locked -p orna-cli-v1 -- init
cargo run --locked -p orna-cli-v1 -- init ./my-project
cargo run --locked -p orna-cli-v1 -- check
cargo run --locked -p orna-cli-v1 -- status
```

`init` preserves existing source and repository metadata. `check` reports
source, resolution, and type failures without replacing the working tree.

### Run and invoke

Run a reachable project entry point, or invoke a reachable zero-argument pure
function:

```sh
cargo run --locked -p orna-cli-v1 -- run
cargo run --locked -p orna-cli-v1 -- run main.main
cargo run --locked -p orna-cli-v1 -- invoke library.value
```

The bounded implementation reports unsupported declarations and operations as
diagnostics; it does not claim the complete language or every runtime workflow.

### REPL

Start an interactive local session or evaluate one expression:

```sh
cargo run --locked -p orna-cli-v1 -- repl
cargo run --locked -p orna-cli-v1 -- repl 'let n: Int = 21;'
```

REPL evaluation is bounded and read-only for previews. A failed submission
does not replace the last successful result or retained declarations.

## Development checks

Run focused checks for the package or the editor protocol surface:

```sh
cargo test --locked -p orna-cli-v1
cargo test --locked -p orna-lsp
```

The LSP binary is built from `crates/orna-lsp` and communicates over standard
input and output with UTF-16 positions. Its tests exercise diagnostics,
symbols, navigation, completion, hover, and semantic tokens without requiring
a running database.

## Project layout

The workspace contains the Orna 1.0 language, value, evaluator, runtime,
repository, storage, system, serving, CLI, and conformance packages. The
canonical specification and its publication digest are kept in
`reference/Orna-1.0.0/`.

## License

OrnaDB is licensed under the [Apache License 2.0](LICENSE).