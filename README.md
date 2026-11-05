# OrnaDB

OrnaDB is a Git-backed database platform for typed applications. The current
implementation work targets the Orna 1.1.0 language, local repository, embedded
runtime, and command-line workflows. The workspace package version is still 1.0.0.

## Current local CLI

The active local binary is `orna`. It works with a local Git-backed
project and keeps source checking, execution, and interactive evaluation on
the same Orna 1.1.0 path.

### Build

Use a Rust 1.95 (or newer) toolchain and Git:

```sh
cargo fetch --locked
cargo build --locked -p orna-cli
```

### Initialize and check a project

Initialize a repository in the current directory or at an explicit path, then
check its reachable Orna source modules:

```sh
cargo run --locked -p orna-cli -- init
cargo run --locked -p orna-cli -- init ./my-project
cargo run --locked -p orna-cli -- check
cargo run --locked -p orna-cli -- status
```

`init` preserves existing source and repository metadata. `check` reports
source, resolution, and type failures without replacing the working tree.

### Run and invoke

Run a reachable project entry point, or invoke a reachable zero-argument pure
function:

```sh
cargo run --locked -p orna-cli -- run
cargo run --locked -p orna-cli -- run main.main
cargo run --locked -p orna-cli -- invoke library.value
```

The bounded implementation reports unsupported declarations and operations as
diagnostics; it does not claim the complete language or every runtime workflow.

### REPL

Start an interactive local session or evaluate one expression:

```sh
cargo run --locked -p orna-cli -- repl
cargo run --locked -p orna-cli -- repl 'let n: Int = 21;'
```

REPL evaluation is bounded and read-only for previews. A failed submission
does not replace the last successful result or retained declarations.

## Development checks

Run focused checks for the package or the editor protocol surface:

```sh
cargo test --locked -p orna-cli
cargo test --locked -p orna-lsp
```

The LSP binary is built from `crates/orna-lsp` and communicates over standard
input and output with UTF-16 positions. Its tests exercise diagnostics,
symbols, navigation, completion, hover, and semantic tokens without requiring
a running database.

The editor packages that launch it live under `editors/` — VS Code, Sublime
Text, Vim, Neovim, and Emacs — and every one of them starts `orna-lsp` by
default with a single override. The start contract, the one override per
editor, and the rule that every file under `editors/` is generated are in the
[editor tooling guide](docs/editor-tooling.md).

## Project layout

The workspace contains the Orna 1.1.0 language, value, evaluator, runtime,
repository, storage, system, serving, CLI, and conformance packages. The
canonical specification and its publication digest are kept in
`reference/Orna-1.1.0/`.

## License

OrnaDB is licensed under the [Apache License 2.0](LICENSE).