# Orna for VS Code

This extension adds language support for [Orna](https://github.com/workingdir/ornadb), including TextMate syntax highlighting and integration with the Orna language server.

## Install for development

1. Open `editors/vscode/` as a folder in VS Code.
2. Press `F5` to launch an Extension Development Host.
3. Open an `.orna` file in the development host.

To install a packaged extension, run **Extensions: Install from VSIX...** in VS Code and select the generated `.vsix` file.

## Configure the language server

The extension starts `orna-lsp` by default. Set `orna.lsp.path` in VS Code settings when the binary is not on `PATH`:

```json
{
  "orna.lsp.path": "/path/to/orna-lsp"
}
```

## Features

- Syntax highlighting for Orna keywords, scalar types, literals, comments, strings, numbers, operators, identifiers, and function calls.
- Declaration highlighting for schemas, types, and functions, including qualified names.
- Language-server integration for `.orna` documents.
- Automatic brackets, quotes, and basic block indentation.
