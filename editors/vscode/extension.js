const vscode = require("vscode");
const { LanguageClient } = require("vscode-languageclient/node");

let client;

function reportServerError(error) {
  const reason = error instanceof Error ? error.message : String(error);
  vscode.window.showErrorMessage(
    `Unable to start the Orna language server (${reason}). Install the orna-lsp binary or set "orna.lsp.path" to its path.`,
  );
}

function activate(context) {
  try {
    const configuredPath = vscode.workspace
      .getConfiguration("orna")
      .get("lsp.path");
    const serverPath = configuredPath || "orna-lsp";
    const serverOptions = {
      command: serverPath,
    };
    const clientOptions = {
      documentSelector: [{ language: "orna" }],
    };

    client = new LanguageClient(
      "orna-language-server",
      "Orna Language Server",
      serverOptions,
      clientOptions,
    );
    context.subscriptions.push(client);
    client.start().catch(reportServerError);
  } catch (error) {
    reportServerError(error);
  }
}

function deactivate() {
  if (!client) {
    return undefined;
  }

  const activeClient = client;
  client = undefined;
  return activeClient.stop().catch(reportServerError);
}

module.exports = {
  activate,
  deactivate,
};
