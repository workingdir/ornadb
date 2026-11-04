"use strict";

// Generated from orna-syntax-v1; edit crates/orna-syntax-v1/src/editor.rs instead.
// Server start contract: docs/editor-tooling.md ("One server command").
//
// This extension starts `orna-lsp` and contributes no language logic of its
// own. Diagnostics, hover, navigation, completion, rename and semantic tokens
// are produced by the server; lexical colouring comes from the TextMate
// grammar and language configuration that package.json contributes.

const vscode = require("vscode");
const { LanguageClient } = require("vscode-languageclient/node");

/** The language id shared with the grammar and language contributions. */
const LANGUAGE_ID = "orna";

/**
 * The only setting that overrides the server command. Unset or empty means
 * `["orna-lsp"]`. The value is an argv list: the first element is the
 * executable and the remaining elements are its arguments.
 */
const SERVER_COMMAND_SETTING = "orna.lsp.serverCommand";

/** The command every editor starts by default. */
const DEFAULT_SERVER_COMMAND = ["orna-lsp"];

/** @type {LanguageClient | undefined} */
let client;

/** Bumped on every start so a late failure from a replaced client is ignored. */
let generation = 0;

/** The argv list used to start the server. @returns {string[]} */
function serverCommand() {
  const configured = vscode.workspace
    .getConfiguration()
    .get(SERVER_COMMAND_SETTING);
  if (Array.isArray(configured) && configured.length > 0) {
    return configured.slice();
  }
  return DEFAULT_SERVER_COMMAND.slice();
}

/**
 * One launch configuration. Omitting `transport` selects stdio, the only
 * transport this contract permits, and passes the server no extra argument.
 * @param {string[]} command
 */
function launchOptions(command) {
  const [executable, ...args] = command;
  return { command: executable, args };
}

function startClient() {
  const current = ++generation;
  client = new LanguageClient(
    LANGUAGE_ID,
    "Orna Language Server",
    {
      run: launchOptions(serverCommand()),
      debug: launchOptions(serverCommand()),
    },
    {
      documentSelector: [
        { scheme: "file", language: LANGUAGE_ID },
        { scheme: "untitled", language: LANGUAGE_ID },
      ],
      synchronize: {
        fileEvents: vscode.workspace.createFileSystemWatcher(
          "**/*.orna",
        ),
      },
      // One restart after a crash, not the client's default of five: a server
      // that cannot start will not start on the fifth attempt either.
      connectionOptions: { maxRestartCount: 0 },
    },
  );
  return client.start().catch((error) => {
    if (current !== generation) {
      return;
    }
    // A missing server is not an activation failure: the contributed grammar
    // keeps colouring `.orna` files. Report the command that did not start so
    // the user can put it on PATH or set the override.
    void vscode.window.showErrorMessage(
      `Orna: the language server did not start: ${String(error)}. ` +
        `Put \`${serverCommand()[0]}\` on PATH or set \`${SERVER_COMMAND_SETTING}\`.`,
    );
  });
}

/** Stop the current client, if any; safe to call more than once. */
function stopClient() {
  const stopped = client;
  client = undefined;
  return stopped ? stopped.stop() : Promise.resolve();
}

/** @param {vscode.ExtensionContext} context */
function activate(context) {
  context.subscriptions.push({ dispose: () => void stopClient() });
  context.subscriptions.push(
    vscode.workspace.onDidChangeConfiguration((event) => {
      if (!event.affectsConfiguration(SERVER_COMMAND_SETTING)) {
        return;
      }
      void stopClient().then(startClient);
    }),
  );
  startClient();
}

/** @returns {Promise<void> | undefined} */
function deactivate() {
  generation += 1;
  return stopClient();
}

module.exports = { activate, deactivate };
